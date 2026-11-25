//! FreeRTOS heap counters of a running 1.10 MAIN, stock or patched.
//!
//! Patches change the MAIN image hash, so the counters are guarded by the
//! allocator code itself: `pvPortMalloc` and `vPortFree` must match their
//! 1.10 bytes in guest memory before the counter addresses are trusted.
use crate::qmp::QmpClient;
use firmware_api::{l6, l6max};
use l6fw::Model;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// One allocator routine: name, entry, length and SHA-256 of its bytes.
type Routine = (&'static str, u32, usize, &'static str);

struct Layout {
    /// Usable heap_4 bytes, as the allocator's first call sets them free.
    size: u32,
    /// `xFreeBytesRemaining`, followed by `xMinimumEverFreeBytesRemaining`,
    /// `xNumberOfSuccessfulAllocations` and `xNumberOfSuccessfulFrees`.
    counters: u32,
    allocator: [Routine; 2],
}

const L6: Layout = Layout {
    size: 0x7cff0,
    counters: l6::xFreeBytesRemaining,
    allocator: [
        (
            "pvPortMalloc",
            l6::pvPortMalloc,
            0x1c8,
            "a25462aa95a97b6f238b0aa8f51b2f612f54abb8965a965332e48113ac16bb8f",
        ),
        (
            "vPortFree",
            l6::vPortFree,
            0xd8,
            "f95e7e19bcdde5d236cf123ecaa2e87f1b604a66d5ca32056696e17c9716eada",
        ),
    ],
};

const L6MAX: Layout = Layout {
    size: 0x7cff8,
    counters: l6max::xFreeBytesRemaining,
    allocator: [
        (
            "pvPortMalloc",
            l6max::pvPortMalloc,
            0x1c4,
            "553dd1e9cbb9f58c30b57f91827a1e627fa34e55d1d8419f9a4c09bb0ec77d8e",
        ),
        (
            "vPortFree",
            l6max::vPortFree,
            0xd4,
            "2a2649e2ee66a717ac2051c30853d14190add0aab9dcffcdd253e154d9fcf7d2",
        ),
    ],
};

fn layout(model: Model) -> &'static Layout {
    match model {
        Model::L6 => &L6,
        Model::L6max => &L6MAX,
    }
}

/// Usable heap bytes on a model's 1.10 MAIN.
pub fn size(model: Model) -> u32 {
    layout(model).size
}

/// One live read of the four heap_4 counters. This is not an atomic guest
/// snapshot, and no plausibility or heap-list consistency checks are applied.
/// Allocation and free counters wrap as 32-bit unsigned integers.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
pub struct Counters {
    pub model: Model,
    pub atomic: bool,
    pub revision_guard: String,
    pub free: u32,
    pub minimum_ever_free: u32,
    pub allocations: u32,
    pub frees: u32,
}

impl Counters {
    fn decode(model: Model, bytes: &[u8]) -> Result<Self, String> {
        let word = |offset: usize| -> Result<u32, String> {
            let bytes = bytes
                .get(offset..offset + 4)
                .ok_or("short heap counter read")?;
            Ok(u32::from_le_bytes(bytes.try_into().unwrap()))
        };
        Ok(Self {
            model,
            atomic: false,
            revision_guard: "1.10_pvPortMalloc_vPortFree".into(),
            free: word(0)?,
            minimum_ever_free: word(4)?,
            allocations: word(8)?,
            frees: word(12)?,
        })
    }
}

/// Guarded access to heap counters for a bounded observation in one guest.
/// Recreate the reader after a restart or a firmware change.
pub struct CounterReader<'a> {
    qmp: &'a QmpClient,
    model: Model,
}

impl CounterReader<'_> {
    /// Perform one QMP read of the four consecutive counter words.
    pub fn read(&self) -> Result<Counters, String> {
        let bytes = self
            .qmp
            .read_memory(0, layout(self.model).counters, 16)
            .map_err(|error| error.to_string())?;
        Counters::decode(self.model, &bytes)
    }
}

fn verify_allocator(
    model: Model,
    mut read: impl FnMut(u32, usize) -> Result<Vec<u8>, String>,
) -> Result<(), String> {
    for (name, address, length, expected) in layout(model).allocator {
        let code = read(address, length)?;
        if format!("{:x}", Sha256::digest(&code)) != expected {
            return Err(format!(
                "{name} at {address:#010x} differs from 1.10; heap counters are unknown for this image"
            ));
        }
    }
    Ok(())
}

/// Verify both allocator routines before trusting the model-specific counter
/// addresses. A patch replacing either routine is not supported.
pub fn counter_reader(qmp: &QmpClient, model: Model) -> Result<CounterReader<'_>, String> {
    verify_allocator(model, |address, length| {
        qmp.read_memory(0, address, length)
            .map_err(|error| error.to_string())
    })?;
    Ok(CounterReader { qmp, model })
}

/// Verify the allocator and take a single live counter sample.
pub fn counters(qmp: &QmpClient, model: Model) -> Result<Counters, String> {
    counter_reader(qmp, model)?.read()
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct Heap {
    pub size: u32,
    pub free: u32,
    pub minimum_ever_free: u32,
    pub allocations: u32,
    pub frees: u32,
}

impl Heap {
    fn decode(size: u32, counters: &[u8]) -> Result<Self, String> {
        let word = |i: usize| -> Result<u32, String> {
            counters
                .get(i * 4..i * 4 + 4)
                .map(|b| u32::from_le_bytes(b.try_into().unwrap()))
                .ok_or_else(|| "short heap counter read".to_string())
        };
        let heap = Self {
            size,
            free: word(0)?,
            minimum_ever_free: word(1)?,
            allocations: word(2)?,
            frees: word(3)?,
        };
        if heap.free > heap.size || heap.minimum_ever_free > heap.free {
            return Err(format!("implausible heap counters: {heap:?}"));
        }
        Ok(heap)
    }

    /// Bytes allocated at the lowest point since boot.
    pub fn peak_used(&self) -> u32 {
        self.size - self.minimum_ever_free
    }
}

/// Verify the model's 1.10 allocator in guest memory, then read its counters.
/// A patch that replaces either allocator routine is refused.
pub fn read(qmp: &QmpClient, model: Model) -> Result<Heap, String> {
    let layout = layout(model);
    verify_allocator(model, |address, length| {
        qmp.read_memory(0, address, length)
            .map_err(|e| e.to_string())
    })?;
    let counters = qmp
        .read_memory(0, layout.counters, 16)
        .map_err(|e| e.to_string())?;
    Heap::decode(layout.size, &counters)
}

#[cfg(test)]
mod tests {
    use super::{CounterReader, Counters, Heap, layout, verify_allocator};
    use crate::qmp::QmpClient;
    use l6fw::Model;
    use std::{
        io::{BufRead, BufReader, Write},
        os::unix::net::UnixStream,
        thread,
    };

    #[test]
    fn decodes_and_checks_counters() {
        let bytes: Vec<u8> = [160_008u32, 150_000, 197, 3]
            .iter()
            .flat_map(|v| v.to_le_bytes())
            .collect();
        let heap = Heap::decode(0x7cff8, &bytes).unwrap();
        assert_eq!(heap.peak_used(), 361_992);
        assert_eq!(heap.allocations, 197);
        let inverted: Vec<u8> = [100u32, 200, 0, 0]
            .iter()
            .flat_map(|v| v.to_le_bytes())
            .collect();
        assert!(Heap::decode(0x7cff8, &inverted).is_err());
        assert!(Heap::decode(0x7cff8, &bytes[..12]).is_err());
    }

    #[test]
    fn counters_decode_raw_values_without_plausibility_restrictions() {
        let bytes: Vec<u8> = [100u32, 200, u32::MAX, 7]
            .into_iter()
            .flat_map(u32::to_le_bytes)
            .collect();
        let counters = Counters::decode(Model::L6max, &bytes).unwrap();
        assert_eq!((counters.free, counters.minimum_ever_free), (100, 200));
        assert_eq!((counters.allocations, counters.frees), (u32::MAX, 7));
        assert!(!counters.atomic);
        assert!(Counters::decode(Model::L6max, &bytes[..12]).is_err());
    }

    #[test]
    fn allocator_guard_uses_model_addresses_and_stops_before_counter_read() {
        for model in [Model::L6, Model::L6max] {
            let mut reads = Vec::new();
            let error = verify_allocator(model, |address, length| {
                reads.push((address, length));
                Ok(vec![0; length])
            })
            .unwrap_err();
            assert!(error.contains("differs from 1.10"));
            assert_eq!(
                reads,
                vec![(layout(model).allocator[0].1, layout(model).allocator[0].2)]
            );
            assert!(
                reads
                    .iter()
                    .all(|(address, _)| *address != layout(model).counters)
            );
        }
        assert_ne!(layout(Model::L6).counters, layout(Model::L6max).counters);
    }

    #[test]
    fn guarded_reader_samples_four_words_in_one_qmp_read() {
        let (client, mut peer) = UnixStream::pair().unwrap();
        let server = thread::spawn(move || {
            let mut input = BufReader::new(peer.try_clone().unwrap());
            peer.write_all(b"{\"QMP\":{\"version\":{\"qemu\":{\"major\":10,\"minor\":0,\"micro\":0},\"package\":\"\"},\"capabilities\":[]}}\r\n").unwrap();
            let mut line = String::new();
            input.read_line(&mut line).unwrap();
            assert_eq!(
                serde_json::from_str::<serde_json::Value>(&line).unwrap()["execute"],
                "qmp_capabilities"
            );
            peer.write_all(b"{\"return\":{}}\r\n").unwrap();
            line.clear();
            input.read_line(&mut line).unwrap();
            let request: serde_json::Value = serde_json::from_str(&line).unwrap();
            assert_eq!(request["execute"], "human-monitor-command");
            assert_eq!(
                request["arguments"]["command-line"],
                format!("xp /16bx 0x{:x}", layout(Model::L6max).counters)
            );
            let response = serde_json::json!({"return":"00000000: 0x01 0x00 0x00 0x00 0x02 0x00 0x00 0x00 0x03 0x00 0x00 0x00 0x04 0x00 0x00 0x00\n"});
            writeln!(peer, "{response}").unwrap();
        });
        let qmp = QmpClient::connect(client).unwrap();
        // Construct directly to isolate the per-sample contract; guard rejection
        // is tested separately above.
        let reader = CounterReader {
            qmp: &qmp,
            model: Model::L6max,
        };
        let counters = reader.read().unwrap();
        assert_eq!(
            (
                counters.free,
                counters.minimum_ever_free,
                counters.allocations,
                counters.frees
            ),
            (1, 2, 3, 4)
        );
        server.join().unwrap();
    }
}
