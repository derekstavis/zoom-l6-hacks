//! Ordered USB mass-storage observations and FAT roundtrip.
use super::*;
use std::io::Read;
pub(super) struct Scope {
    pub phase: u8,
    start: Instant,
    guest_start: u32,
    guest_budget: u32,
    wall_budget: u64,
    session: l6max_host::usb_storage::Session,
    inquiry: Vec<u8>,
    blocks: u64,
    size: u32,
    boot: Option<[u8; 512]>,
    original: Option<[u8; 512]>,
    reread: Option<[u8; 512]>,
    filename: String,
    expected: Vec<u8>,
    actual: Vec<u8>,
}
impl Scope {
    pub(super) fn begin(s: &Session, guest: u32, wall: u64) -> Result<Self, String> {
        let start = Instant::now();
        let deadline = start
            .checked_add(Duration::from_secs(wall))
            .ok_or("USB storage wall deadline overflow")?;
        let ctl = l6ctl::OwnedControl::new(&s.engine);
        let guest_start = ctl
            .input_status(l6ctl::InputSource::Panel)
            .map_err(|e| e.to_string())?
            .guest_ms;
        let session = ctl.usb_storage_before(deadline, guest_start, guest)?;
        Ok(Self {
            phase: 0,
            start,
            guest_start,
            guest_budget: guest,
            wall_budget: wall,
            session,
            inquiry: vec![],
            blocks: 0,
            size: 0,
            boot: None,
            original: None,
            reread: None,
            filename: format!("L6T{:05X}.TXT", std::process::id() & 0xfffff),
            expected: vec![],
            actual: vec![],
        })
    }
    pub(super) fn check(&self, s: &Session, g: u32, w: u64) -> Result<(), String> {
        let now = l6ctl::OwnedControl::new(&s.engine)
            .input_status(l6ctl::InputSource::Panel)
            .map_err(|e| e.to_string())?
            .guest_ms;
        if (g, w) != (self.guest_budget, self.wall_budget)
            || self.start.elapsed() >= Duration::from_secs(w)
            || now.wrapping_sub(self.guest_start) > g
        {
            Err("USB storage shared deadline exceeded or changed".into())
        } else {
            Ok(())
        }
    }
    pub(super) fn inquiry(&mut self) -> Result<Value, String> {
        self.inquiry = self.session.inquiry().map_err(|e| e.to_string())?;
        Ok(json!({"inquiry":String::from_utf8_lossy(&self.inquiry[8..36])}))
    }
    pub(super) fn ready(&mut self) -> Result<Value, String> {
        self.session.ready().map_err(|e| e.to_string())?;
        Ok(json!({"ready":true}))
    }
    pub(super) fn capacity(&mut self) -> Result<Value, String> {
        (self.blocks, self.size) = self.session.capacity().map_err(|e| e.to_string())?;
        if self.size != 512 || self.blocks == 0 {
            return Err("invalid sector geometry".into());
        }
        Ok(json!({"blocks":self.blocks,"sector_size":self.size}))
    }
    pub(super) fn read_boot(&mut self) -> Result<Value, String> {
        self.boot = Some(self.session.read_sector(0).map_err(|e| e.to_string())?);
        Ok(json!({"lba":0}))
    }
    pub(super) fn assert_boot(&self, o: &Options) -> Result<Value, String> {
        let mut expected = [0; 512];
        fs::File::open(
            o.sd_image
                .as_ref()
                .ok_or("USB storage requires an SD card")?,
        )
        .map_err(|e| e.to_string())?
        .read_exact(&mut expected)
        .map_err(|e| e.to_string())?;
        if self.boot.as_ref() != Some(&expected) {
            return Err("USB sector did not match attached SD image".into());
        }
        Ok(json!({"matches_card":true}))
    }
    pub(super) fn last_read(&mut self) -> Result<Value, String> {
        let lba = u32::try_from(self.blocks.checked_sub(1).ok_or("missing capacity")?)
            .map_err(|_| "last LBA exceeds READ(10)")?;
        self.original = Some(self.session.read_sector(lba).map_err(|e| e.to_string())?);
        Ok(json!({"lba":lba}))
    }
    pub(super) fn last_write(&mut self) -> Result<Value, String> {
        let lba = u32::try_from(self.blocks - 1).map_err(|_| "last LBA exceeds WRITE(10)")?;
        self.session
            .write_sector(
                lba,
                self.original.as_ref().ok_or("missing original sector")?,
            )
            .map_err(|e| e.to_string())?;
        Ok(json!({"lba":lba}))
    }
    pub(super) fn last_assert(&mut self) -> Result<Value, String> {
        let lba = u32::try_from(self.blocks - 1).map_err(|_| "last LBA exceeds READ(10)")?;
        self.reread = Some(self.session.read_sector(lba).map_err(|e| e.to_string())?);
        if self.reread != self.original {
            return Err("USB sector write/read mismatch".into());
        }
        Ok(json!({"sector_round_trip":true}))
    }
    pub(super) fn fat_open(&mut self) -> Result<Value, String> {
        self.session.open_fat().map_err(|e| e.to_string())?;
        Ok(json!({"opened":true}))
    }
    pub(super) fn create(
        &mut self,
        length: u32,
        multiplier: u32,
        increment: u32,
    ) -> Result<Value, String> {
        if (length, multiplier, increment) != (1537, 37, 11) {
            return Err("USB storage fixture requires the verified 1537-byte pattern".into());
        }
        self.expected = (0..length)
            .map(|i| ((i * multiplier + increment) % 256) as u8)
            .collect();
        self.session
            .create_file(&self.filename, &self.expected)
            .map_err(|e| e.to_string())?;
        Ok(json!({"filename":self.filename,"bytes":length}))
    }
    pub(super) fn flush(&mut self) -> Result<Value, String> {
        self.session.flush().map_err(|e| e.to_string())?;
        Ok(json!({"flushed":true}))
    }
    pub(super) fn read(&mut self) -> Result<Value, String> {
        self.actual = self
            .session
            .read_file(&self.filename)
            .map_err(|e| e.to_string())?;
        Ok(json!({"bytes":self.actual.len()}))
    }
    pub(super) fn remove(&mut self) -> Result<Value, String> {
        self.session
            .remove_file(&self.filename)
            .map_err(|e| e.to_string())?;
        Ok(json!({"removed":true}))
    }
    pub(super) fn assert_file(&self) -> Result<Value, String> {
        if self.actual != self.expected {
            return Err(format!(
                "USB FAT file roundtrip mismatch: wrote {} bytes, read {} bytes",
                self.expected.len(),
                self.actual.len()
            ));
        }
        Ok(json!({"matched":true}))
    }
    pub(super) fn finish(&mut self) -> Result<Value, String> {
        self.session.finish().map_err(|e| e.to_string())?;
        Ok(
            json!({"inquiry":String::from_utf8_lossy(&self.inquiry[8..36]),"blocks":self.blocks,"sector_size":self.size,"sector_round_trip":true,"fat_file_round_trip_bytes":self.expected.len()}),
        )
    }
    pub(super) fn abort(&mut self) -> Result<(), String> {
        self.session
            .abort(Instant::now() + Duration::from_secs(3))
            .map_err(|e| e.to_string())
    }
}
