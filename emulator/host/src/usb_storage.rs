//! USB Bulk-Only Transport. SCSI requests are executed by firmware, including SD DMA.
use crate::{
    sector_device::{SECTOR, SectorDevice, SectorDisk},
    usb::{Budget, Endpoints, UsbHost},
};
use std::{io, sync::Arc};

/// A SCSI command that completed with a failed status, with its sense data
/// when the device reported CHECK CONDITION.
#[derive(Debug)]
pub struct CommandFailed {
    pub opcode: u8,
    pub status: u8,
    pub residue: usize,
    /// Sense key, additional sense code and qualifier.
    pub sense: Option<[u8; 3]>,
}
impl std::fmt::Display for CommandFailed {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "SCSI opcode {:02x} failed, status {}, residue {}",
            self.opcode, self.status, self.residue
        )?;
        if let Some([key, asc, ascq]) = self.sense {
            write!(f, ", sense key {key:x} ASC {asc:02x} ASCQ {ascq:02x}")?;
        }
        Ok(())
    }
}
impl std::error::Error for CommandFailed {}

enum UsbRef<'a> {
    Borrowed(&'a UsbHost),
    Owned(Arc<UsbHost>),
}
impl std::ops::Deref for UsbRef<'_> {
    type Target = UsbHost;
    fn deref(&self) -> &UsbHost {
        match self {
            Self::Borrowed(v) => v,
            Self::Owned(v) => v,
        }
    }
}
pub struct MassStorage<'a> {
    usb: UsbRef<'a>,
    endpoints: Endpoints,
    tag: u32,
    budget: Option<Budget>,
}
impl<'a> MassStorage<'a> {
    pub fn new(usb: &'a UsbHost, endpoints: Endpoints) -> Self {
        Self {
            usb: UsbRef::Borrowed(usb),
            endpoints,
            tag: 0,
            budget: None,
        }
    }
    fn send(&self, endpoint: u8, data: &[u8]) -> io::Result<()> {
        match &self.budget {
            Some(b) => self.usb.send_before(endpoint, data, b),
            None => self.usb.send(endpoint, data),
        }
    }
    fn receive(&self, endpoint: u8, length: usize) -> io::Result<Vec<u8>> {
        match &self.budget {
            Some(b) => self.usb.receive_before(endpoint, length, b),
            None => self.usb.receive(endpoint, length),
        }
    }
    fn command(&mut self, cdb: &[u8], length: usize, output: Option<&[u8]>) -> io::Result<Vec<u8>> {
        if cdb.is_empty()
            || cdb.len() > 16
            || length > 16384
            || output.is_some_and(|d| d.len() != length)
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "invalid SCSI transfer",
            ));
        }
        self.tag = self.tag.wrapping_add(1).max(1);
        let mut cbw = [0u8; 31];
        cbw[..4].copy_from_slice(b"USBC");
        cbw[4..8].copy_from_slice(&self.tag.to_le_bytes());
        cbw[8..12].copy_from_slice(&(length as u32).to_le_bytes());
        cbw[12] = if output.is_none() { 0x80 } else { 0 };
        cbw[14] = cdb.len() as u8;
        cbw[15..15 + cdb.len()].copy_from_slice(cdb);
        self.send(self.endpoints.output, &cbw)?;
        let mut input = Vec::new();
        if length != 0 {
            if let Some(data) = output {
                self.send(self.endpoints.output, data)?;
            } else {
                while input.len() < length {
                    let data = self.receive(self.endpoints.input, 16384)?;
                    if data.is_empty() {
                        break;
                    }
                    if input.len() + data.len() > length {
                        return Err(io::Error::new(
                            io::ErrorKind::InvalidData,
                            "SCSI data exceeds expected transfer",
                        ));
                    }
                    input.extend(data);
                }
            }
        }
        let csw = self.receive(self.endpoints.input, 16384)?;
        if csw.len() != 13
            || &csw[..4] != b"USBS"
            || u32::from_le_bytes(csw[4..8].try_into().unwrap()) != self.tag
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("invalid SCSI command status {csw:02x?}"),
            ));
        }
        let residue = u32::from_le_bytes(csw[8..12].try_into().unwrap()) as usize;
        if csw[12] != 0 {
            // TEST UNIT READY callers read the sense themselves; reading it
            // here would clear it first.
            let sense = if csw[12] == 1 && !matches!(cdb[0], 0 | 3) {
                self.request_sense()
                    .ok()
                    .filter(|sense| sense.len() >= 14)
                    .map(|sense| [sense[2] & 15, sense[12], sense[13]])
            } else {
                None
            };
            return Err(io::Error::other(CommandFailed {
                opcode: cdb[0],
                status: csw[12],
                residue,
                sense,
            }));
        }
        if residue != 0 || (output.is_none() && input.len() != length) {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "short SCSI transfer",
            ));
        }
        Ok(input)
    }
    pub fn inquiry(&mut self) -> io::Result<Vec<u8>> {
        self.command(&[0x12, 0, 0, 0, 36, 0], 36, None)
    }
    pub fn request_sense(&mut self) -> io::Result<Vec<u8>> {
        self.command(&[3, 0, 0, 0, 18, 0], 18, None)
    }
    /// Wait while the device reports UNIT ATTENTION or becoming ready. The
    /// sense data is the signal; the wall ceiling only bounds a stuck device.
    pub fn ready(&mut self) -> io::Result<()> {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
        while std::time::Instant::now() < deadline {
            match self.command(&[0, 0, 0, 0, 0, 0], 0, None) {
                Ok(_) => return Ok(()),
                Err(e) if e.kind() == io::ErrorKind::Other => {
                    let sense = self.request_sense()?;
                    let key = sense[2] & 15;
                    if key != 6 && !(key == 2 && sense[12] == 4) {
                        return Err(io::Error::other(format!(
                            "SCSI not ready: sense key {key:x}, ASC {:02x}, ASCQ {:02x}",
                            sense[12], sense[13]
                        )));
                    }
                    std::thread::sleep(std::time::Duration::from_millis(100));
                }
                Err(e) => return Err(e),
            }
        }
        Err(io::Error::new(
            io::ErrorKind::TimedOut,
            "SCSI readiness deadline",
        ))
    }
    pub fn capacity(&mut self) -> io::Result<(u64, u32)> {
        let data = self.command(&[0x25, 0, 0, 0, 0, 0, 0, 0, 0, 0], 8, None)?;
        Ok((
            u32::from_be_bytes(data[..4].try_into().unwrap()) as u64 + 1,
            u32::from_be_bytes(data[4..8].try_into().unwrap()),
        ))
    }
    pub fn read_sector(&mut self, lba: u32) -> io::Result<[u8; 512]> {
        let mut cdb = [0u8; 10];
        cdb[0] = 0x28;
        cdb[2..6].copy_from_slice(&lba.to_be_bytes());
        cdb[8] = 1;
        self.command(&cdb, 512, None)?
            .try_into()
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "sector length"))
    }
    pub fn write_sector(&mut self, lba: u32, bytes: &[u8; 512]) -> io::Result<()> {
        let mut cdb = [0u8; 10];
        cdb[0] = 0x2a;
        cdb[2..6].copy_from_slice(&lba.to_be_bytes());
        cdb[8] = 1;
        self.command(&cdb, 512, Some(bytes)).map(|_| ())
    }
    pub fn read_sectors(&mut self, lba: u32, bytes: &mut [u8]) -> io::Result<()> {
        let mut cdb = [0u8; 10];
        cdb[0] = 0x28;
        cdb[2..6].copy_from_slice(&lba.to_be_bytes());
        cdb[7..9].copy_from_slice(&((bytes.len() / 512) as u16).to_be_bytes());
        let data = self.command(&cdb, bytes.len(), None)?;
        if data.len() != bytes.len() {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "sector length"));
        }
        bytes.copy_from_slice(&data);
        Ok(())
    }
    pub fn write_sectors(&mut self, lba: u32, bytes: &[u8]) -> io::Result<()> {
        let mut cdb = [0u8; 10];
        cdb[0] = 0x2a;
        cdb[2..6].copy_from_slice(&lba.to_be_bytes());
        cdb[7..9].copy_from_slice(&((bytes.len() / 512) as u16).to_be_bytes());
        self.command(&cdb, bytes.len(), Some(bytes)).map(|_| ())
    }
    /// Flush the device cache. A device without SYNCHRONIZE CACHE rejects it
    /// as an invalid opcode; like host operating systems, treat that as
    /// having no cache to flush.
    pub fn synchronize(&mut self) -> io::Result<()> {
        match self.command(&[0x35, 0, 0, 0, 0, 0, 0, 0, 0, 0], 0, None) {
            Err(error)
                if error
                    .get_ref()
                    .and_then(|inner| inner.downcast_ref::<CommandFailed>())
                    .is_some_and(|failed| failed.sense == Some([5, 0x20, 0])) =>
            {
                Ok(())
            }
            result => result.map(|_| ()),
        }
    }
}

impl MassStorage<'static> {
    pub fn owned(usb: Arc<UsbHost>, endpoints: Endpoints, budget: Budget) -> Self {
        Self {
            usb: UsbRef::Owned(usb),
            endpoints,
            tag: 0,
            budget: Some(budget),
        }
    }
}

/// A USB mass-storage device as a [`SectorDevice`]. The firmware owns every
/// read and write; this only issues SCSI requests.
pub struct UsbSectors<'a> {
    disk: MassStorage<'a>,
    blocks: u64,
}
impl<'a> UsbSectors<'a> {
    pub fn open(mut disk: MassStorage<'a>) -> io::Result<Self> {
        disk.ready()?;
        let (blocks, size) = disk.capacity()?;
        if size as usize != SECTOR || blocks > u32::MAX as u64 {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "USB FAT access requires 512-byte sectors and READ(10) addressing",
            ));
        }
        Ok(Self { disk, blocks })
    }
}
impl SectorDevice for UsbSectors<'_> {
    fn sector_count(&self) -> u64 {
        self.blocks
    }
    fn read_sector(&mut self, lba: u32) -> io::Result<[u8; SECTOR]> {
        self.disk.read_sector(lba)
    }
    fn write_sector(&mut self, lba: u32, bytes: &[u8; SECTOR]) -> io::Result<()> {
        self.disk.write_sector(lba, bytes)
    }
    fn read_sectors(&mut self, lba: u32, bytes: &mut [u8]) -> io::Result<()> {
        self.disk.read_sectors(lba, bytes)
    }
    fn write_sectors(&mut self, lba: u32, bytes: &[u8]) -> io::Result<()> {
        self.disk.write_sectors(lba, bytes)
    }
    fn flush(&mut self) -> io::Result<()> {
        self.disk.synchronize()
    }
}

/// Seekable FAT volume over the emulated USB disk.
pub type UsbDisk<'a> = SectorDisk<UsbSectors<'a>>;
impl<'a> SectorDisk<UsbSectors<'a>> {
    pub fn new(disk: MassStorage<'a>) -> io::Result<Self> {
        Self::open(UsbSectors::open(disk)?)
    }
}

trait CleanupVolume {
    fn names(&mut self) -> io::Result<Vec<String>>;
    fn remove_owned(&mut self, name: &str) -> io::Result<()>;
    fn flush_owned(&mut self) -> io::Result<()>;
}
impl CleanupVolume for sd_image::Volume<UsbDisk<'static>> {
    fn names(&mut self) -> io::Result<Vec<String>> {
        Ok(self
            .entries("/")?
            .into_iter()
            .map(|entry| entry.name)
            .collect())
    }
    fn remove_owned(&mut self, name: &str) -> io::Result<()> {
        self.remove(name)
    }
    fn flush_owned(&mut self) -> io::Result<()> {
        self.flush()
    }
}
fn cleanup_pending(
    volume: &mut impl CleanupVolume,
    pending: &mut Option<String>,
) -> io::Result<()> {
    if let Some(name) = pending.as_deref() {
        if volume
            .names()?
            .iter()
            .any(|entry| entry.eq_ignore_ascii_case(name))
        {
            volume.remove_owned(name)?;
        }
        volume.flush_owned()?;
        *pending = None;
    }
    Ok(())
}

/// Exclusive owned USB mass-storage transaction. A single SCSI tag stream is
/// retained through block and FAT operations. Explicitly finish or abort it.
pub struct Session {
    disk: Option<MassStorage<'static>>,
    volume: Option<sd_image::Volume<UsbDisk<'static>>>,
    budget: Budget,
    _lease: crate::usb::DirectLease,
    pending_file: Option<String>,
    file_removed: bool,
}
impl Session {
    pub fn open(usb: Arc<UsbHost>, budget: Budget) -> io::Result<Self> {
        let lease = usb.acquire_direct().map_err(io::Error::other)?;
        let setup = (|| {
            let config = usb.enumerate_before(&budget)?;
            config
                .storage
                .ok_or_else(|| io::Error::other("firmware did not expose mass storage"))
        })();
        let endpoints = match setup {
            Ok(endpoints) => endpoints,
            Err(error) => {
                budget
                    .cleanup_before(std::time::Instant::now() + std::time::Duration::from_secs(3));
                return match lease.rollback_before(&usb, &budget) {
                    Ok(()) => Err(error),
                    Err(cleanup) => {
                        usb.shutdown();
                        Err(io::Error::other(format!(
                            "{error}; USB setup rollback failed: {cleanup}; transport closed, restart required"
                        )))
                    }
                };
            }
        };
        Ok(Self {
            disk: Some(MassStorage::owned(usb, endpoints, budget.clone())),
            volume: None,
            budget,
            _lease: lease,
            pending_file: None,
            file_removed: false,
        })
    }
    fn disk(&mut self) -> io::Result<&mut MassStorage<'static>> {
        self.budget.check()?;
        self.disk
            .as_mut()
            .ok_or_else(|| io::Error::other("storage block phase is not active"))
    }
    fn volume(&mut self) -> io::Result<&mut sd_image::Volume<UsbDisk<'static>>> {
        self.budget.check()?;
        self.volume
            .as_mut()
            .ok_or_else(|| io::Error::other("storage FAT phase is not active"))
    }
    pub fn inquiry(&mut self) -> io::Result<Vec<u8>> {
        self.disk()?.inquiry()
    }
    pub fn ready(&mut self) -> io::Result<()> {
        self.disk()?.ready()
    }
    pub fn capacity(&mut self) -> io::Result<(u64, u32)> {
        self.disk()?.capacity()
    }
    pub fn read_sector(&mut self, lba: u32) -> io::Result<[u8; 512]> {
        self.disk()?.read_sector(lba)
    }
    pub fn write_sector(&mut self, lba: u32, data: &[u8; 512]) -> io::Result<()> {
        self.disk()?.write_sector(lba, data)
    }
    pub fn open_fat(&mut self) -> io::Result<()> {
        self.budget.check()?;
        if self.volume.is_some() {
            return Err(io::Error::other("FAT volume already open"));
        }
        let disk = self
            .disk
            .take()
            .ok_or_else(|| io::Error::other("storage block phase is not active"))?;
        self.volume = Some(sd_image::Volume::open(UsbDisk::new(disk)?)?);
        self.budget.check()
    }
    pub fn create_file(&mut self, name: &str, bytes: &[u8]) -> io::Result<()> {
        if self.pending_file.is_some() {
            return Err(io::Error::other("storage file already pending"));
        }
        if self
            .volume()?
            .entries("/")?
            .iter()
            .any(|entry| entry.name.eq_ignore_ascii_case(name))
        {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                "USB FAT roundtrip filename already exists",
            ));
        }
        self.budget.check()?;
        self.pending_file = Some(name.into());
        self.volume()?.write(name, bytes)
    }
    pub fn read_file(&mut self, name: &str) -> io::Result<Vec<u8>> {
        if self.pending_file.as_deref() != Some(name) {
            return Err(io::Error::other(
                "file is not owned by this storage session",
            ));
        }
        self.volume()?.read(name)
    }
    pub fn remove_file(&mut self, name: &str) -> io::Result<()> {
        if self.pending_file.as_deref() != Some(name) {
            return Err(io::Error::other(
                "file is not owned by this storage session",
            ));
        }
        self.volume()?.remove(name)?;
        self.file_removed = true;
        Ok(())
    }
    pub fn flush(&mut self) -> io::Result<()> {
        self.volume()?.flush()?;
        if self.file_removed {
            self.pending_file = None;
            self.file_removed = false;
        }
        Ok(())
    }
    pub fn finish(&mut self) -> io::Result<()> {
        self.budget.check()?;
        if self.pending_file.is_some() {
            return Err(io::Error::other("storage file cleanup is incomplete"));
        }
        Ok(())
    }
    /// Attempt file removal using a separate wall-bounded cleanup budget.
    pub fn abort(&mut self, deadline: std::time::Instant) -> io::Result<()> {
        self.budget.cleanup_before(deadline);
        if self.pending_file.is_some() {
            let volume = self.volume.as_mut().ok_or_else(|| {
                io::Error::other("cannot clean up pending FAT file without a volume")
            })?;
            cleanup_pending(volume, &mut self.pending_file)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod session_tests {
    use super::*;
    use std::{
        io::{Read, Write},
        os::unix::net::UnixStream,
        thread,
        time::{Duration, Instant},
    };
    #[derive(Default)]
    struct FakeVolume {
        names: Vec<String>,
        operations: Vec<String>,
        fail_flush: bool,
    }
    impl CleanupVolume for FakeVolume {
        fn names(&mut self) -> io::Result<Vec<String>> {
            Ok(self.names.clone())
        }
        fn remove_owned(&mut self, name: &str) -> io::Result<()> {
            self.operations.push(format!("remove:{name}"));
            self.names.clear();
            Ok(())
        }
        fn flush_owned(&mut self) -> io::Result<()> {
            self.operations.push("flush".into());
            if self.fail_flush {
                Err(io::Error::other("injected flush failure"))
            } else {
                Ok(())
            }
        }
    }
    #[test]
    fn cleanup_removes_only_owned_file_and_retains_ownership_until_flush() {
        let mut volume = FakeVolume {
            names: vec!["OWNED.TXT".into()],
            fail_flush: true,
            ..Default::default()
        };
        let mut pending = Some("owned.txt".into());
        assert!(cleanup_pending(&mut volume, &mut pending).is_err());
        assert_eq!(pending.as_deref(), Some("owned.txt"));
        volume.fail_flush = false;
        cleanup_pending(&mut volume, &mut pending).unwrap();
        assert!(pending.is_none());
        assert_eq!(volume.operations, ["remove:owned.txt", "flush", "flush"]);
        let mut unrelated = FakeVolume {
            names: vec!["OTHER.TXT".into()],
            ..Default::default()
        };
        cleanup_pending(&mut unrelated, &mut None).unwrap();
        assert!(unrelated.operations.is_empty());
    }
    fn setup_failure(raw: bool) {
        let (socket, mut peer) = UnixStream::pair().unwrap();
        let worker = thread::spawn(move || {
            for (kind, endpoint, status) in if raw {
                vec![(1u32, 1u32, 0i32), (2, 0, -libc::EINVAL)]
            } else {
                vec![(1, 1, 0), (2, 0, -libc::EINVAL), (1, 0, 0)]
            } {
                let mut request = [0; 20];
                peer.read_exact(&mut request).unwrap();
                let word =
                    |i: usize| u32::from_le_bytes(request[i * 4..i * 4 + 4].try_into().unwrap());
                assert_eq!((word(1), word(3)), (kind, endpoint));
                if kind == 2 {
                    let mut setup = [0; 8];
                    peer.read_exact(&mut setup).unwrap();
                }
                let mut reply = Vec::new();
                for value in [0x4c365553u32, 0x80000000, word(2), status as u32, 0] {
                    reply.extend(value.to_le_bytes())
                }
                peer.write_all(&reply).unwrap();
            }
        });
        let host = Arc::new(UsbHost::new(socket).unwrap());
        if raw {
            *host.ownership().lock().unwrap() = crate::usb::UsbOwner::Raw;
        }
        let budget = Budget::new(Instant::now() + Duration::from_secs(1), || false);
        assert!(Session::open(host.clone(), budget).is_err());
        assert_eq!(
            *host.ownership().lock().unwrap(),
            if raw {
                crate::usb::UsbOwner::Raw
            } else {
                crate::usb::UsbOwner::None
            }
        );
        worker.join().unwrap();
    }
    #[test]
    fn failed_setup_detaches_only_a_new_attachment() {
        setup_failure(false);
        setup_failure(true);
    }
}
