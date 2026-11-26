//! Shared scenario definition, execution and evidence for headless and GPUI tests.
pub mod devices;
pub mod firmware;
pub mod fixture_layout;
pub mod host_midi;
pub mod progress;
pub mod requirements;
mod runner;
mod scenario;
mod session;
mod trace;
use l6ctl::audio_analysis as waveform;
pub use runner::{Progress, Runner};
pub use scenario::{
    Action, AudioInput, Backend, Fragment, Gesture, ObservationActivity, STATE_SPEC_ENV, Scenario,
    Step, UpdateFixture, control_target, is_switch, spec_path,
};
pub use session::{Command, Session, run, run_supervised_din};
