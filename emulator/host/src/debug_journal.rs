//! Journal and cleanup ordering for a retained debugger transaction.
//!
//! This type records state only: it does not provide guest writes or acquire a
//! debugger lease. Callers must still own and guard their transport and targets.
use std::io;

pub struct Journal {
    pub writes: Vec<Write>,
    pub breakpoints: Vec<u32>,
    pub registers: Vec<(u32, u32)>,
    pub synchronized: bool,
    pub mailbox_pending: bool,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Write {
    pub address: u32,
    pub before: Vec<u8>,
    pub after: Vec<u8>,
}
impl Default for Journal {
    fn default() -> Self {
        Self {
            writes: Vec::new(),
            breakpoints: Vec::new(),
            registers: Vec::new(),
            synchronized: true,
            mailbox_pending: false,
        }
    }
}
impl Journal {
    /// A failed restoration precondition leaves all records in place and
    /// forbids further cleanup writes or resume until the guest is restarted.
    pub fn restoration_guard(&mut self, result: Result<(), String>) -> Result<(), String> {
        if result.is_err() {
            self.synchronized = false;
        }
        result
    }
    /// Record before attempting a write, including a write that may partially fail.
    pub fn begin_write(
        &mut self,
        address: u32,
        before: Vec<u8>,
        data: &[u8],
    ) -> io::Result<Option<usize>> {
        let end = u64::from(address) + data.len() as u64;
        let mut restoring = None;
        for (index, write) in self.writes.iter().enumerate() {
            let other_end = u64::from(write.address) + write.before.len() as u64;
            if u64::from(address) < other_end && u64::from(write.address) < end {
                if write.address == address
                    && write.before.as_slice() == data
                    && restoring.is_none()
                {
                    restoring = Some(index);
                } else {
                    return Err(io::Error::other("overlapping temporary debugger writes"));
                }
            }
        }
        self.writes.push(Write {
            address,
            before,
            after: data.to_vec(),
        });
        Ok(restoring)
    }
    pub fn commit_write(&mut self, restoring: Option<usize>) {
        if let Some(index) = restoring {
            self.writes.pop();
            self.writes.remove(index);
        }
    }
    pub fn handoff_mailbox(&mut self, address: u32) {
        self.writes.retain(|write| write.address != address);
        self.mailbox_pending = true;
    }
    pub fn acknowledge_mailbox(&mut self) {
        self.mailbox_pending = false;
    }
    pub fn clean(&self) -> bool {
        self.writes.is_empty()
            && self.breakpoints.is_empty()
            && self.registers.is_empty()
            && self.synchronized
            && !self.mailbox_pending
    }
}

/// Host-defined cleanup order; a failed stage stops further guest mutations.
pub trait Cleanup {
    fn stop(&mut self) -> Result<(), String>;
    fn synchronized(&self) -> bool;
    fn restore_writes(&mut self) -> Vec<String>;
    fn clear_breakpoints(&mut self) -> Vec<String>;
    fn restore_registers(&mut self) -> Vec<String>;
    fn resume(&mut self) -> Result<(), String>;
}
pub fn cleanup(backend: &mut impl Cleanup) -> Vec<String> {
    let mut failures = Vec::new();
    if let Err(error) = backend.stop() {
        return vec![format!("stop: {error}; guest state unknown")];
    }
    if !backend.synchronized() {
        return vec![
            "debugger transport or mailbox handoff not synchronized; guest left stopped".into(),
        ];
    }
    failures.extend(backend.restore_writes());
    if failures.is_empty() {
        failures.extend(backend.clear_breakpoints());
    }
    if failures.is_empty() {
        failures.extend(backend.restore_registers());
    }
    if !failures.is_empty() {
        failures.push("guest left stopped".into());
        return failures;
    }
    if let Err(error) = backend.resume() {
        failures.push(format!("resume: {error}"));
    }
    failures
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn journal_tracks_pending_restore_and_rejects_overlaps() {
        let mut j = Journal::default();
        j.begin_write(0x100, vec![1, 2], &[3, 4]).unwrap();
        assert!(j.begin_write(0x101, vec![0], &[7]).is_err());
        let restore = j.begin_write(0x100, vec![3, 4], &[1, 2]).unwrap();
        assert_eq!(j.writes.len(), 2);
        j.commit_write(restore);
        assert!(j.clean());
    }
    #[test]
    fn mailbox_handoff_retains_uncertainty_until_acknowledged() {
        let mut j = Journal::default();
        j.begin_write(0x100, vec![255], &[12]).unwrap();
        j.handoff_mailbox(0x100);
        assert!(!j.clean());
        j.acknowledge_mailbox();
        assert!(j.clean());
    }
    #[test]
    fn failed_restoration_guard_retains_journal_and_prevents_cleanup_mutations() {
        struct Fake {
            journal: Journal,
            calls: Vec<&'static str>,
        }
        impl Cleanup for Fake {
            fn stop(&mut self) -> Result<(), String> {
                self.calls.push("stop");
                Ok(())
            }
            fn synchronized(&self) -> bool {
                self.journal.synchronized && !self.journal.mailbox_pending
            }
            fn restore_writes(&mut self) -> Vec<String> {
                self.calls.push("write");
                vec![]
            }
            fn clear_breakpoints(&mut self) -> Vec<String> {
                self.calls.push("breakpoint");
                vec![]
            }
            fn restore_registers(&mut self) -> Vec<String> {
                self.calls.push("register");
                vec![]
            }
            fn resume(&mut self) -> Result<(), String> {
                self.calls.push("resume");
                Ok(())
            }
        }
        let mut fake = Fake {
            journal: Journal::default(),
            calls: vec![],
        };
        fake.journal.begin_write(0x100, vec![1], &[0]).unwrap();
        assert!(
            fake.journal
                .restoration_guard(Err("changed slot".into()))
                .is_err()
        );
        let errors = cleanup(&mut fake);
        assert_eq!(fake.calls, ["stop"]);
        assert_eq!(fake.journal.writes[0].address, 0x100);
        assert_eq!(fake.journal.writes[0].before, [1]);
        assert!(errors.iter().any(|e| e.contains("guest left stopped")));
    }
    #[test]
    fn failed_write_restoration_stops_before_other_cleanup_or_resume() {
        #[derive(Default)]
        struct Fake {
            calls: Vec<&'static str>,
        }
        impl Cleanup for Fake {
            fn stop(&mut self) -> Result<(), String> {
                self.calls.push("stop");
                Ok(())
            }
            fn synchronized(&self) -> bool {
                true
            }
            fn restore_writes(&mut self) -> Vec<String> {
                self.calls.push("writes");
                vec!["injected restore failure".into()]
            }
            fn clear_breakpoints(&mut self) -> Vec<String> {
                self.calls.push("breakpoints");
                Vec::new()
            }
            fn restore_registers(&mut self) -> Vec<String> {
                self.calls.push("registers");
                Vec::new()
            }
            fn resume(&mut self) -> Result<(), String> {
                self.calls.push("resume");
                Ok(())
            }
        }
        let mut backend = Fake::default();
        let errors = cleanup(&mut backend);
        assert_eq!(backend.calls, ["stop", "writes"]);
        assert!(
            errors
                .iter()
                .any(|e| e.contains("injected restore failure"))
        );
        assert!(errors.iter().any(|e| e.contains("guest left stopped")));
    }
}
