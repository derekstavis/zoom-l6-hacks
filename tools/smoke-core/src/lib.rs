//! Shared scenario definition, execution and evidence for headless and GPUI tests.
pub mod devices;
pub mod host_midi;
mod runner;
mod scenario;
mod session;
mod trace;
mod waveform;
pub use runner::{Progress, Runner};
pub use scenario::{
    Action, AudioInput, Gesture, STATE_SPEC_ENV, Scenario, Step, control_target, is_switch,
    model_name,
};
pub use session::{Command, Session, run};
