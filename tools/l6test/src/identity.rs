//! What a case ran on, recorded in its result so a pass names its exact
//! inputs. Nothing is decided from it.
use serde::Serialize;

/// Hashes of the binaries and fixtures a case ran on.
#[derive(Clone, Debug, Serialize)]
pub struct CaseIdentity {
    pub qemu_sha256: String,
    /// The executable that ran the scenario.
    pub runner_sha256: String,
    /// The package the case boots or installs from, if any.
    pub firmware_package_sha256: Option<String>,
    /// The unpacked firmware directory the case boots.
    pub firmware: String,
    pub card_sha256: Option<String>,
    pub state: String,
    /// Audio fed to each input channel, by channel.
    pub audio_input: Vec<(u8, String)>,
}
