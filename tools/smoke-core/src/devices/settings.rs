//! Frozen persistence observations. The byte-level checks match the firmware's
//! existing emulator scenario; the first valid marker is intentionally not
//! l6nor's active-bank selection.
use super::*;

pub(super) struct Scope {
    restored: bool,
    rtc: Vec<u8>,
    panel: Vec<u8>,
    nor: Vec<u8>,
    trace: Option<String>,
    bank: Option<usize>,
    panel_start: u32,
    guest: u32,
    deadline: Instant,
}
fn rtc_words(bytes: &[u8]) -> Result<(u32, u32), String> {
    let time = u32::from_le_bytes(bytes.get(..4).ok_or("short RTC file")?.try_into().unwrap());
    let calendar = u32::from_le_bytes(bytes.get(4..8).ok_or("short RTC file")?.try_into().unwrap());
    Ok((time, calendar))
}
fn first_valid_bank(nor: &[u8]) -> Option<usize> {
    [0x1f9000usize, 0x1fc000].into_iter().find(|&base| {
        nor.get(base..base + 4)
            .is_some_and(|b| (1..=3).contains(&u32::from_le_bytes(b.try_into().unwrap())))
    })
}
fn timestamp(trace: &str, prefix: &str) -> Option<u32> {
    trace.lines().find_map(|line| {
        line.strip_prefix(prefix)?
            .split_whitespace()
            .next()?
            .parse()
            .ok()
    })
}
fn edited_rtc_matches(bytes: &[u8]) -> Result<bool, String> {
    let (time, calendar) = rtc_words(bytes)?;
    Ok(calendar & 0x00ff1f3f == 0x00260202 && time & 0x003f7f00 == 0x00010100)
}
fn restored_rtc_matches(before: &[u8], after: &[u8]) -> Result<bool, String> {
    let (before_time, _) = rtc_words(before)?;
    let (after_time, _) = rtc_words(after)?;
    Ok(before[4..8] == after[4..8] && before_time & 0x003f7f00 == after_time & 0x003f7f00)
}
fn rtc_response_within_deadline(request: u32, response: u32) -> bool {
    response >= request && response - request < 2000
}
impl Scope {
    fn check(&self, session: &Session) -> Result<(), String> {
        if Instant::now() >= self.deadline
            || guest_now(session).wrapping_sub(self.panel_start) > self.guest
        {
            Err("settings observation shared deadline exceeded".into())
        } else {
            Ok(())
        }
    }
    pub(super) fn capture(
        session: &Session,
        options: &Options,
        restored: bool,
        guest: u32,
        wall: u64,
    ) -> Result<Self, String> {
        let panel_start = guest_now(session);
        let deadline = Instant::now()
            .checked_add(Duration::from_secs(wall))
            .ok_or("settings deadline overflow")?;
        let state = options
            .state_dir
            .as_ref()
            .ok_or("settings assertion requires persistent state")?;
        let rtc = fs::read(state.join("panel-rtc.bin")).map_err(|e| e.to_string())?;
        let panel = fs::read(state.join("panel-flash.bin")).map_err(|e| e.to_string())?;
        let nor = fs::read(state.join("main-nor.bin")).map_err(|e| e.to_string())?;
        if session.engine.model() != l6fw::Model::L6max {
            return Err("settings layout currently supports L6max 1.10 only".into());
        }
        let scope = Self {
            restored,
            rtc,
            panel,
            nor,
            trace: None,
            bank: None,
            panel_start,
            guest,
            deadline,
        };
        scope.check(session)?;
        Ok(scope)
    }
    pub(super) fn versions(&self, session: &Session) -> Result<Value, String> {
        self.check(session)?;
        if self.panel.get(0x3ffc..0x4000) != Some(&l6fw::STOCK_PANEL_VERSION)
            || self.nor.get(0x1f7ffc..0x1f8000) != Some(&l6fw::STOCK_VERSION)
            || self.nor.get(0x1f6ffc..0x1f7000) != Some(&l6fw::STOCK_PANEL_VERSION)
        {
            return Err("persisted firmware version records do not match".into());
        }
        Ok(json!({"versions_match":true}))
    }
    pub(super) fn rtc(
        &self,
        session: &Session,
        saved: Option<&(Vec<u8>, usize)>,
    ) -> Result<Value, String> {
        self.check(session)?;
        rtc_words(&self.rtc)?;
        if self.restored {
            let (before, _) = saved.ok_or("restore assertion needs saved-settings checkpoint")?;
            if !restored_rtc_matches(before, &self.rtc)? {
                return Err("RTC fields changed across restart".into());
            }
        } else if !edited_rtc_matches(&self.rtc)? {
            return Err("date editor did not save all five fields".into());
        }
        Ok(json!({"restored":self.restored,"rtc":self.rtc}))
    }
    pub(super) fn bank(&mut self, session: &Session) -> Result<Value, String> {
        self.check(session)?;
        if self.restored {
            return Err("cannot select bank after restart".into());
        }
        let bank = first_valid_bank(&self.nor).ok_or("settings bank not committed")?;
        self.bank = Some(bank);
        Ok(json!({"first_valid_bank":bank}))
    }
    pub(super) fn trace(&mut self, session: &Session, options: &Options) -> Result<Value, String> {
        self.check(session)?;
        if !self.restored {
            return Err("trace needs restored settings".into());
        }
        let mut trace =
            fs::read_to_string(options.logs.join("engine.trace")).map_err(|e| e.to_string())?;
        if let Ok(panel) = fs::read_to_string(options.logs.join("engine-panel.trace")) {
            trace.push_str(&panel);
        }
        self.check(session)?;
        self.trace = Some(trace);
        Ok(json!({"captured":true}))
    }
    pub(super) fn panel(&self, session: &Session) -> Result<Value, String> {
        self.check(session)?;
        let trace = self.trace.as_ref().ok_or("settings trace missing")?;
        if trace.contains("l6 panel ROM write") {
            return Err("panel reprogrammed on restart".into());
        }
        Ok(json!({"panel_reprogrammed":false}))
    }
    pub(super) fn reload(
        &self,
        session: &Session,
        saved: Option<&(Vec<u8>, usize)>,
    ) -> Result<Value, String> {
        self.check(session)?;
        let (_, bank) = saved.ok_or("restore assertion needs saved-settings checkpoint")?;
        let trace = self.trace.as_ref().ok_or("settings trace missing")?;
        if !trace.contains(&format!("addr={:08x} ipcr1=000017e0", bank + 4)) {
            return Err("settings bank was not reloaded".into());
        }
        Ok(json!({"bank":bank}))
    }
    pub(super) fn rtc_timing(&self, session: &Session) -> Result<Value, String> {
        self.check(session)?;
        let trace = self.trace.as_ref().ok_or("settings trace missing")?;
        let request = timestamp(trace, "l6 main RTC request at ").ok_or("missing RTC request")?;
        let response =
            timestamp(trace, "l6 panel RTC response start at ").ok_or("missing RTC response")?;
        if !rtc_response_within_deadline(request, response) {
            return Err("RTC response missed startup deadline".into());
        }
        Ok(json!({"request":request,"response":response}))
    }
    pub(super) fn finish(&self, session: &Session) -> Result<Value, String> {
        self.check(session)?;
        Ok(json!({"restored":self.restored}))
    }
    pub(super) fn saved_checkpoint(self) -> Result<Option<(Vec<u8>, usize)>, String> {
        if self.restored {
            Ok(None)
        } else {
            Ok(Some((self.rtc, self.bank.ok_or("settings bank missing")?)))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn first_valid_bank_preserves_slot_order() {
        let mut nor = vec![0; 0x1fc004];
        nor[0x1f9000..0x1f9004].copy_from_slice(&1u32.to_le_bytes());
        nor[0x1fc000..0x1fc004].copy_from_slice(&2u32.to_le_bytes());
        assert_eq!(first_valid_bank(&nor), Some(0x1f9000));
        nor[0x1f9000..0x1f9004].copy_from_slice(&4u32.to_le_bytes());
        assert_eq!(first_valid_bank(&nor), Some(0x1fc000));
        assert_eq!(first_valid_bank(&nor[..8]), None);
    }
    #[test]
    fn rtc_accepts_at_least_eight_bytes_and_timestamps_use_first_match() {
        assert!(rtc_words(&[0; 7]).is_err());
        assert_eq!(rtc_words(&[0; 9]).unwrap(), (0, 0));
        assert_eq!(
            timestamp(
                "l6 main RTC request at 12\nl6 main RTC request at 14\n",
                "l6 main RTC request at "
            ),
            Some(12)
        );
    }
    #[test]
    fn rtc_masks_and_response_boundary_match_legacy() {
        let mut rtc = Vec::new();
        rtc.extend(0x00010159u32.to_le_bytes());
        rtc.extend(0x00260202u32.to_le_bytes());
        assert!(edited_rtc_matches(&rtc).unwrap());
        let mut after = rtc.clone();
        after[0] = 0;
        assert!(restored_rtc_matches(&rtc, &after).unwrap());
        after[1] ^= 1;
        assert!(!restored_rtc_matches(&rtc, &after).unwrap());
        assert!(rtc_response_within_deadline(100, 2099));
        assert!(!rtc_response_within_deadline(100, 2100));
        assert!(!rtc_response_within_deadline(100, 99));
    }
}
