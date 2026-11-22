//! Version-1 SD module files, as the host tools write and read them.
use alloc::{vec, vec::Vec};

use super::{
    Error, FORMAT_VERSION, HEADER_LEN_V1, HOOK_RECORD_LEN, Hook, LOADER_ABI, MAX_HOOKS_PER_MODULE,
    MAX_MEMORY_LEN, MAX_RELOCATIONS_PER_MODULE, RELOCATION_RECORD_LEN, checksum, read_body,
    read_header,
};
use crate::relocation::Relocation;

/// One SD module: its code as linked at offset zero, the zero-filled state
/// that follows it, its function-entry hooks and its relocations.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Module {
    pub code: Vec<u8>,
    /// Code plus zero-filled state. The loader allocates this much; only
    /// `code` is stored in the file.
    pub memory_len: usize,
    /// Hooks are stored by address. Hooks on the same address keep their
    /// order, which is the order of their chain.
    pub hooks: Vec<Hook>,
    pub relocations: Vec<Relocation>,
}

impl Module {
    /// Encode the module, refusing anything the loader would reject.
    pub fn encode(&self) -> Result<Vec<u8>, &'static str> {
        let code_len = self.code.len();
        if code_len == 0 || self.memory_len < code_len || self.memory_len > MAX_MEMORY_LEN {
            return Err("invalid SD module code or memory length");
        }
        if self.hooks.is_empty() || self.hooks.len() > MAX_HOOKS_PER_MODULE {
            return Err("an SD module needs between 1 and 1024 hooks");
        }
        if self.relocations.len() > MAX_RELOCATIONS_PER_MODULE {
            return Err("an SD module has at most 256 relocations");
        }
        let mut hooks = self.hooks.clone();
        hooks.sort_by_key(|hook| hook.address);
        if hooks.iter().any(|hook| {
            hook.address & 1 != 0
                || hook.entry_offset & 1 != 0
                || hook.entry_offset as usize >= code_len
                || (hook.next_slot_offset != u32::MAX
                    && (hook.next_slot_offset & 3 != 0
                        || hook.next_slot_offset as usize + 4 > code_len))
        }) {
            return Err("invalid function-entry hook");
        }
        for relocation in &self.relocations {
            let [kind, first, second] = relocation.record();
            let mut offsets = [Some(first), (kind == 2).then_some(second)].into_iter();
            if offsets.any(|offset| offset.is_some_and(|o| o & 1 != 0 || o as usize + 4 > code_len))
            {
                return Err("relocation outside or misaligned in patch code");
            }
            if kind == 2 && (second <= first || second - first > 16) {
                return Err("invalid Thumb MOVW/MOVT relocation pair");
            }
        }

        let mut output = Vec::with_capacity(
            HEADER_LEN_V1
                + code_len
                + hooks.len() * HOOK_RECORD_LEN
                + self.relocations.len() * RELOCATION_RECORD_LEN,
        );
        output.extend_from_slice(b"L6SB");
        output.extend_from_slice(&FORMAT_VERSION.to_le_bytes());
        output.extend_from_slice(&LOADER_ABI.to_le_bytes());
        output.extend_from_slice(&(code_len as u32).to_le_bytes());
        output.extend_from_slice(&(self.memory_len as u32).to_le_bytes());
        output.extend_from_slice(&(hooks.len() as u16).to_le_bytes());
        output.extend_from_slice(&(self.relocations.len() as u16).to_le_bytes());
        output.extend_from_slice(&0u32.to_le_bytes()); // reserved
        output.extend_from_slice(&[0; 4]); // checksum
        output.extend_from_slice(&self.code);
        for hook in &hooks {
            for word in [hook.address, hook.entry_offset, hook.next_slot_offset] {
                output.extend_from_slice(&word.to_le_bytes());
            }
        }
        for relocation in &self.relocations {
            for word in relocation.record() {
                output.extend_from_slice(&word.to_le_bytes());
            }
        }
        let crc = checksum(&output).ok_or("short SD module")?;
        output[24..28].copy_from_slice(&crc.to_le_bytes());
        Ok(output)
    }

    /// Decode a module with the checks and limits the loader applies.
    pub fn decode(file: &[u8]) -> Result<Self, Error> {
        let mut reader = file;
        let pending = read_header(
            &mut reader,
            MAX_MEMORY_LEN,
            MAX_HOOKS_PER_MODULE,
            MAX_RELOCATIONS_PER_MODULE,
        )?;
        let header = pending.header;
        let mut code = vec![0; header.memory_len];
        let mut hooks = vec![Hook::default(); header.hook_count];
        read_body(&mut reader, pending, &mut code, &mut hooks, 0, 0)?;
        code.truncate(header.code_len);
        let records = HEADER_LEN_V1 + header.code_len + header.hook_count * HOOK_RECORD_LEN;
        let word = |record: &[u8], index: usize| {
            u32::from_le_bytes(record[index * 4..index * 4 + 4].try_into().unwrap()) as usize
        };
        let relocations = file[records..]
            .chunks_exact(RELOCATION_RECORD_LEN)
            .map(|record| match word(record, 0) {
                1 => Relocation::Abs32 {
                    offset: word(record, 1),
                },
                _ => Relocation::ThumbMov32 {
                    low_offset: word(record, 1),
                    high_offset: word(record, 2),
                },
            })
            .collect();
        Ok(Module {
            code,
            memory_len: header.memory_len,
            hooks,
            relocations,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hook(address: u32, entry_offset: u32, next_slot_offset: u32) -> Hook {
        Hook {
            address,
            entry_offset,
            next_slot_offset,
        }
    }

    fn module(code: &[u8], memory_len: usize, hooks: &[Hook]) -> Module {
        Module {
            code: code.to_vec(),
            memory_len,
            hooks: hooks.to_vec(),
            relocations: Vec::new(),
        }
    }

    fn u32_at(file: &[u8], offset: usize) -> u32 {
        u32::from_le_bytes(file[offset..offset + 4].try_into().unwrap())
    }

    #[test]
    fn encodes_the_header_code_and_hooks() {
        let code = [0x00, 0xbf, 0x70, 0x47, 0, 0, 0, 0];
        let file = module(&code, 0x48000, &[hook(0x8000_1000, 0, 4)])
            .encode()
            .unwrap();
        assert_eq!(file.len(), HEADER_LEN_V1 + code.len() + HOOK_RECORD_LEN);
        assert_eq!(&file[..8], b"L6SB\x01\x00\x01\x00");
        assert_eq!(u32_at(&file, 8), 8);
        assert_eq!(u32_at(&file, 12), 0x48000);
        assert_eq!(&file[16..20], &[1, 0, 0, 0]);
        assert_eq!(&file[28..36], &code);
        assert_eq!(u32_at(&file, 36), 0x8000_1000);
        assert_eq!(checksum(&file), Some(u32_at(&file, 24)));
        let mut corrupt = file;
        corrupt[25] ^= 1;
        assert_ne!(checksum(&corrupt), Some(u32_at(&corrupt, 24)));
    }

    #[test]
    fn round_trips_and_keeps_the_order_of_hooks_on_one_function() {
        let mut code = [0u8; 16];
        // Thumb MOVW r0, #8 followed by MOVT r0, #0.
        code[8..16].copy_from_slice(&[0x40, 0xf2, 0x08, 0x00, 0xc0, 0xf2, 0x00, 0x00]);
        let original = Module {
            code: code.to_vec(),
            memory_len: 32,
            hooks: vec![
                hook(0x8000_2000, 0, u32::MAX),
                hook(0x8000_1000, 4, 12),
                hook(0x8000_1000, 0, 4),
            ],
            relocations: vec![Relocation::ThumbMov32 {
                low_offset: 8,
                high_offset: 12,
            }],
        };
        let file = original.encode().unwrap();
        let decoded = Module::decode(&file).unwrap();
        assert_eq!(
            decoded.hooks,
            [
                hook(0x8000_1000, 4, 12),
                hook(0x8000_1000, 0, 4),
                hook(0x8000_2000, 0, u32::MAX)
            ]
        );
        assert_eq!(decoded.code, original.code);
        assert_eq!(decoded.relocations, original.relocations);
        assert_eq!(decoded.encode().unwrap(), file);
    }

    #[test]
    fn refuses_what_the_loader_rejects() {
        let code = [0u8; 8];
        let entry = [hook(0x8000_1000, 0, 4)];
        assert!(module(&code, MAX_MEMORY_LEN + 1, &entry).encode().is_err());
        assert!(module(&code, 4, &entry).encode().is_err());
        assert!(module(&code, 8, &[]).encode().is_err());
        assert!(
            module(&code, 8, &[hook(0x8000_1001, 0, 4)])
                .encode()
                .is_err()
        );
        assert!(
            module(&code, 8, &[hook(0x8000_1000, 0, 6)])
                .encode()
                .is_err()
        );
        let mut far = module(&code, 8, &entry);
        far.relocations.push(Relocation::Abs32 { offset: 6 });
        assert!(far.encode().is_err());
    }
}
