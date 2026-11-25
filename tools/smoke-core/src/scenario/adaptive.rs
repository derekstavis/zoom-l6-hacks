//! Narrow, bounded plans for the knob-dialog adaptive demo.
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Deserialize, Serialize, schemars::JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Plan {
    pub blocks: Vec<Block>,
}

#[derive(Clone, Debug, Deserialize, Serialize, schemars::JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct DemoCapture {
    pub guest_ms: u32,
    pub min_lit: u32,
    pub checked_frames: u32,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, schemars::JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum Child {
    CaptureDemo,
    AssertCenter,
    SavePan,
    ReadPan,
    ReadText,
    CheckTarget,
    TurnPan,
    Capture,
    Tap,
    WaitMode,
    ReadChannel1,
    ReadChannel2,
    TurnChannel2,
    CheckChannel2,
    AssertLevel,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, schemars::JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Exhaustion {
    Fail,
    Continue,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, schemars::JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Direction {
    Left,
    Right,
}

#[derive(Clone, Debug, Deserialize, Serialize, schemars::JsonSchema, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "kebab-case", deny_unknown_fields)]
pub enum Block {
    Center {
        children: Vec<Child>,
        artifact: String,
        demo: DemoCapture,
    },
    PanSweep {
        children: Vec<Child>,
        after: Vec<Child>,
        direction: Direction,
        target: u8,
        min_percent: u8,
        max_attempts: u32,
        exhaustion: Exhaustion,
        artifact_prefix: String,
        endpoint_artifact: String,
        endpoint_guest_ms: u32,
        demo: DemoCapture,
    },
    PanReset {
        children: Vec<Child>,
        max_attempts: u32,
        exhaustion: Exhaustion,
        artifact: String,
        guest_ms: u32,
    },
    SelectLevel {
        children: Vec<Child>,
        target: u32,
        hold_ms: u32,
        mode: u8,
    },
    Channel2 {
        before: Vec<Child>,
        attempt: Vec<Child>,
        after: Vec<Child>,
        deltas: Vec<i32>,
        artifact: String,
        guest_ms: u32,
    },
}

fn artifact(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 64
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}
fn demo(value: &DemoCapture) -> bool {
    value.guest_ms > 0
        && value.guest_ms <= 180_000
        && value.min_lit > 0
        && value.min_lit <= 8192
        && value.checked_frames > 0
        && value.checked_frames <= 100
}
impl Plan {
    pub fn validate(&self) -> Result<(), String> {
        use Child as C;
        if self.blocks.len() != 6 {
            return Err(
                "adaptive plan requires center, left, right, reset, level and channel blocks"
                    .into(),
            );
        }
        if !matches!(&self.blocks[0], Block::Center { children, artifact: name, demo: d } if children == &[C::CaptureDemo,C::AssertCenter,C::SavePan] && artifact(name) && demo(d))
        {
            return Err("invalid center block".into());
        }
        for (index, direction) in [(1, Direction::Left), (2, Direction::Right)] {
            if !matches!(&self.blocks[index], Block::PanSweep { children, after, direction: dir, min_percent, max_attempts, exhaustion, artifact_prefix, endpoint_artifact, endpoint_guest_ms, demo: d, .. } if children == &[C::ReadPan,C::ReadText,C::CheckTarget,C::TurnPan,C::CaptureDemo] && after == &[C::Capture] && *dir == direction && *min_percent <= 100 && *max_attempts > 0 && *max_attempts <= 100 && *exhaustion == Exhaustion::Fail && artifact(artifact_prefix) && artifact_prefix.len() <= 61 && artifact(endpoint_artifact) && *endpoint_guest_ms > 0 && *endpoint_guest_ms <= 180_000 && demo(d))
            {
                return Err("invalid pan sweep".into());
            }
        }
        if !matches!(&self.blocks[3], Block::PanReset { children,max_attempts,exhaustion,artifact: name,guest_ms } if children == &[C::ReadPan,C::CheckTarget,C::TurnPan,C::Capture] && *max_attempts > 0 && *max_attempts <= 100 && *exhaustion == Exhaustion::Continue && artifact(name) && *guest_ms > 0 && *guest_ms <= 180_000)
        {
            return Err("invalid pan reset".into());
        }
        if !matches!(&self.blocks[4], Block::SelectLevel { children,target,hold_ms,mode } if children == &[C::Tap,C::WaitMode] && l6ctl::button_source(*target).is_some() && *hold_ms > 0 && *hold_ms <= 60_000 && *mode == 9)
        {
            return Err("invalid level selection".into());
        }
        if !matches!(&self.blocks[5], Block::Channel2 { before,attempt,after,deltas,artifact: name,guest_ms } if before == &[C::ReadChannel1,C::ReadChannel2] && attempt == &[C::TurnChannel2,C::Capture,C::ReadChannel2,C::CheckChannel2] && after == &[C::ReadText,C::AssertLevel] && !deltas.is_empty() && deltas.len() <= 100 && deltas.iter().all(|d| *d != 0 && d.unsigned_abs() <= 1000) && artifact(name) && *guest_ms > 0 && *guest_ms <= 180_000)
        {
            return Err("invalid channel-2 block".into());
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn plan() -> Plan {
        let fragment: crate::Fragment = toml::from_str(include_str!(
            "../../../../patches/knob-dialog/tests/emulator/fragments/invalid-tags.toml"
        ))
        .unwrap();
        match &fragment.step[0].action {
            crate::Action::KnobDialogAdaptive { plan } => plan.clone(),
            _ => panic!("missing plan"),
        }
    }
    #[test]
    fn canonical_plan_is_bounded_and_valid() {
        plan().validate().unwrap();
    }
    #[test]
    fn rejects_missing_producers_and_unbounded_attempts() {
        let mut p = plan();
        if let Block::PanSweep { children, .. } = &mut p.blocks[1] {
            children.remove(0);
        }
        assert!(p.validate().is_err());
        let mut p = plan();
        if let Block::PanReset { max_attempts, .. } = &mut p.blocks[3] {
            *max_attempts = 101;
        }
        assert!(p.validate().is_err());
        let mut p = plan();
        if let Block::Channel2 { deltas, .. } = &mut p.blocks[5] {
            deltas.clear();
        }
        assert!(p.validate().is_err());
    }
}
