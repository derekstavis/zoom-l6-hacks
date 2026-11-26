//! Bounded coordinated pause of an engine's MAIN and panel QEMU instances.
use crate::qmp::QmpClient;
use std::time::{Duration, Instant};

trait Guest: Send {
    fn running_before(&self, deadline: Instant) -> Result<bool, String>;
    fn stop_before(&self, deadline: Instant) -> Result<(), String>;
    fn resume_before(&self, deadline: Instant) -> Result<(), String>;
}
impl Guest for QmpClient {
    fn running_before(&self, d: Instant) -> Result<bool, String> {
        QmpClient::running_before(self, d).map_err(|e| e.to_string())
    }
    fn stop_before(&self, d: Instant) -> Result<(), String> {
        QmpClient::stop_before(self, d).map_err(|e| e.to_string())
    }
    fn resume_before(&self, d: Instant) -> Result<(), String> {
        QmpClient::resume_before(self, d).map_err(|e| e.to_string())
    }
}
/// Explicitly close this scope and inspect errors. A failed restore leaves the
/// affected guest's state unknown and requires restart before further use.
pub struct PauseScope {
    guests: Vec<(Box<dyn Guest>, bool)>,
    closed: bool,
}
impl PauseScope {
    pub fn acquire(
        clients: &[QmpClient],
        deadline: Instant,
        cancelled: impl FnMut() -> bool,
    ) -> Result<Self, String> {
        if clients.len() != 2 {
            return Err("coordinated pause requires MAIN and panel QMP clients".into());
        }
        Self::acquire_inner(
            clients
                .iter()
                .cloned()
                .map(|c| Box::new(c) as Box<dyn Guest>)
                .collect(),
            deadline,
            cancelled,
        )
    }
    fn acquire_inner(
        clients: Vec<Box<dyn Guest>>,
        deadline: Instant,
        mut cancelled: impl FnMut() -> bool,
    ) -> Result<Self, String> {
        let mut scope = Self {
            guests: vec![],
            closed: false,
        };
        for client in clients {
            if cancelled() || Instant::now() >= deadline {
                return Err(scope.failed("coordinated pause cancelled or deadline exceeded".into()));
            }
            let running = match client.running_before(deadline) {
                Ok(v) => v,
                Err(e) => return Err(scope.failed(format!("query guest state: {e}"))),
            };
            scope.guests.push((client, running));
            if cancelled() || Instant::now() >= deadline {
                return Err(scope.failed("coordinated pause cancelled or deadline exceeded".into()));
            }
            if let Err(e) = scope.guests.last().unwrap().0.stop_before(deadline) {
                return Err(scope.failed(format!("stop guest: {e}")));
            }
            if cancelled() || Instant::now() >= deadline {
                return Err(scope.failed("coordinated pause cancelled or deadline exceeded".into()));
            }
        }
        Ok(scope)
    }
    fn failed(&mut self, primary: String) -> String {
        match self.close_before(Instant::now() + Duration::from_secs(3)) {
            Ok(()) => primary,
            Err(e) => format!("{primary}; restore failed: {e}"),
        }
    }
    pub fn close_before(&mut self, deadline: Instant) -> Result<(), String> {
        if self.closed {
            return Ok(());
        }
        self.closed = true;
        let mut errors = Vec::new();
        for (index, (guest, running)) in self.guests.iter().enumerate() {
            if *running {
                if let Err(e) = guest.resume_before(deadline).and_then(|()| {
                    match guest.running_before(deadline) {
                        Ok(true) => Ok(()),
                        Ok(false) => Err("guest remains stopped".into()),
                        Err(e) => Err(e),
                    }
                }) {
                    errors.push(format!("guest {index}: {e}"));
                }
            }
        }
        if errors.is_empty() {
            Ok(())
        } else {
            Err(format!(
                "guest state uncertain; restart required: {}",
                errors.join("; ")
            ))
        }
    }
}
impl Drop for PauseScope {
    fn drop(&mut self) {
        if !self.closed {
            let _ = self.close_before(Instant::now() + Duration::from_secs(3));
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};
    #[derive(Default)]
    struct State {
        running: [bool; 2],
        log: Vec<String>,
        fail_stop: Option<usize>,
        fail_resume: Option<usize>,
    }
    struct Fake(usize, Arc<Mutex<State>>);
    impl Guest for Fake {
        fn running_before(&self, _: Instant) -> Result<bool, String> {
            Ok(self.1.lock().unwrap().running[self.0])
        }
        fn stop_before(&self, _: Instant) -> Result<(), String> {
            let mut s = self.1.lock().unwrap();
            s.log.push(format!("stop{}", self.0));
            s.running[self.0] = false;
            if s.fail_stop == Some(self.0) {
                Err("stop reply lost".into())
            } else {
                Ok(())
            }
        }
        fn resume_before(&self, _: Instant) -> Result<(), String> {
            let mut s = self.1.lock().unwrap();
            s.log.push(format!("resume{}", self.0));
            if s.fail_resume == Some(self.0) {
                return Err("resume reply lost".into());
            }
            s.running[self.0] = true;
            Ok(())
        }
    }
    fn guests(s: &Arc<Mutex<State>>) -> Vec<Box<dyn Guest>> {
        (0..2)
            .map(|i| Box::new(Fake(i, s.clone())) as Box<dyn Guest>)
            .collect()
    }
    #[test]
    fn restores_only_initially_running_guests() {
        let s = Arc::new(Mutex::new(State {
            running: [true, false],
            ..Default::default()
        }));
        let d = Instant::now() + Duration::from_secs(1);
        let mut p = PauseScope::acquire_inner(guests(&s), d, || false).unwrap();
        p.close_before(d).unwrap();
        let s = s.lock().unwrap();
        assert_eq!(s.running, [true, false]);
        assert_eq!(s.log, ["stop0", "stop1", "resume0"]);
    }
    #[test]
    fn rolls_back_even_when_stop_reply_is_lost() {
        let s = Arc::new(Mutex::new(State {
            running: [true, true],
            fail_stop: Some(1),
            ..Default::default()
        }));
        assert!(
            PauseScope::acquire_inner(guests(&s), Instant::now() + Duration::from_secs(1), || {
                false
            })
            .is_err()
        );
        assert_eq!(s.lock().unwrap().running, [true, true]);
    }
    #[test]
    fn cancellation_after_first_stop_restores_it_and_does_not_touch_second() {
        let state = Arc::new(Mutex::new(State {
            running: [true, true],
            ..Default::default()
        }));
        let calls = std::cell::Cell::new(0);
        let outcome = PauseScope::acquire_inner(
            guests(&state),
            Instant::now() + Duration::from_secs(1),
            || {
                calls.set(calls.get() + 1);
                calls.get() >= 3
            },
        );
        assert!(outcome.is_err());
        let state = state.lock().unwrap();
        assert_eq!(state.running, [true, true]);
        assert_eq!(state.log, ["stop0", "resume0"]);
    }
    #[test]
    fn reports_uncertain_restore_and_attempts_both_guests() {
        let s = Arc::new(Mutex::new(State {
            running: [true, true],
            fail_resume: Some(0),
            ..Default::default()
        }));
        let d = Instant::now() + Duration::from_secs(1);
        let mut p = PauseScope::acquire_inner(guests(&s), d, || false).unwrap();
        assert!(p.close_before(d).unwrap_err().contains("restart required"));
        assert!(s.lock().unwrap().running[1]);
    }
}
