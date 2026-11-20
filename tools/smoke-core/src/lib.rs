//! Shared scenario definition, execution and evidence for headless and GPUI tests.
pub mod devices;
pub mod firmware;
pub mod fixture_layout;
pub mod host_midi;
pub mod requirements;
mod runner;
mod scenario;
mod session;
mod trace;
mod waveform;
pub use runner::{Progress, Runner};
pub use scenario::{
    Action, AudioInput, Gesture, STATE_SPEC_ENV, Scenario, Step, control_target, is_switch,
    model_name, spec_path,
};
pub use session::{Command, Session, run};
