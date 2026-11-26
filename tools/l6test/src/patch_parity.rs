//! Host-only browser-template/native parity scenario execution.
use l6max_host::engine::Options;
use serde::Serialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use smoke_core::{
    Action, Scenario,
    progress::{CaseEvent, CaseProgress, Outcome, step_label, unix_ms},
};
use std::{
    fs,
    path::{Path, PathBuf},
    time::Instant,
};

#[derive(Serialize)]
struct ProfileEvidence {
    patches: Vec<String>,
    bytes: usize,
    sha256: String,
    usage: String,
}

struct Parity {
    model: l6fw::Model,
    firmware: PathBuf,
    root: PathBuf,
    stock: Vec<u8>,
    profiles: Vec<ProfileEvidence>,
    loader_profiles: Vec<ProfileEvidence>,
}

fn digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn native(
    firmware: &Path,
    output: &Path,
    patches: &[String],
    mode: l6patch::BuildMode,
) -> Result<Vec<u8>, String> {
    l6patch::build(l6patch::BuildRequest {
        source: firmware.to_owned(),
        output: output.to_owned(),
        patches: patches.to_vec(),
        effect_replacements: Vec::new(),
        version: None,
        mode,
    })
    .map_err(|e| e.to_string())?;
    let model = l6fw::model_from_main(
        &fs::read(firmware.join("main_firmware.bin")).map_err(|e| e.to_string())?,
    )
    .map_err(|e| e.to_string())?;
    fs::read(output.join(model.package_file_name())).map_err(|e| e.to_string())
}

fn evidence(patches: &[String], bytes: &[u8], report: &Value) -> ProfileEvidence {
    ProfileEvidence {
        patches: patches.to_vec(),
        bytes: bytes.len(),
        sha256: digest(bytes),
        usage: report["usage"].as_str().unwrap_or("").into(),
    }
}

fn compare_bytes(
    model: l6fw::Model,
    patches: &[String],
    browser: &[u8],
    native: &[u8],
) -> Result<(), String> {
    if browser == native {
        return Ok(());
    }
    Err(format!(
        "{} {patches:?} mismatch: WASM {} bytes SHA-256 {}, native {} bytes SHA-256 {}",
        model.name(),
        browser.len(),
        digest(browser),
        native.len(),
        digest(native)
    ))
}
fn knob_callback(report: &Value) -> Result<Option<u64>, String> {
    let patches = report["patches"]
        .as_array()
        .ok_or("patch report has no patches")?;
    let mut entries = patches
        .iter()
        .filter(|entry| entry["name"].as_str() == Some("knob-dialog"));
    let Some(entry) = entries.next() else {
        return Ok(None);
    };
    if entries.next().is_some() {
        return Err("duplicate knob-dialog report entry".into());
    }
    Ok(Some(
        entry["knob_callback"]
            .as_u64()
            .ok_or("knob callback export missing")?,
    ))
}
fn compare_callback_metadata(native_path: &Path, browser: &Value) -> Result<(), String> {
    let native: Value = serde_json::from_slice(
        &fs::read(native_path.join("patch.json")).map_err(|e| e.to_string())?,
    )
    .map_err(|e| e.to_string())?;
    let a = knob_callback(&native)?;
    let b = knob_callback(browser)?;
    if a != b {
        return Err(format!(
            "native/browser knob callback metadata mismatch: {a:?} != {b:?}"
        ));
    }
    Ok(())
}

fn validate_overlap_rejection(
    patches: &[String],
    native: &str,
    browser: Result<(Vec<u8>, Value), String>,
) -> Result<(), String> {
    match browser {
        Err(error) if error.contains("overlap the Patch loader module reserve") => Ok(()),
        Err(error) => Err(format!(
            "native and WASM reject {patches:?} for different reasons: {native} / {error}"
        )),
        Ok(_) => Err(format!(
            "WASM accepted {patches:?}, which native rejects because its static payload overlaps the SD module region"
        )),
    }
}

impl Parity {
    fn prepare(options: &Options) -> Result<Self, String> {
        let main =
            fs::read(options.firmware_dir.join("main_firmware.bin")).map_err(|e| e.to_string())?;
        let model = l6fw::model_from_main(&main).map_err(|e| e.to_string())?;
        let root = options.logs.join("wasm-patch-lifecycle");
        fs::create_dir_all(&root).map_err(|e| e.to_string())?;
        let path = root.join("stock.bin");
        l6fw::repack(&options.firmware_dir, &path, false).map_err(|e| e.to_string())?;
        let stock = fs::read(path).map_err(|e| e.to_string())?;
        if l6fw::model_from_package(&stock).map_err(|e| e.to_string())? != model {
            return Err(format!(
                "repacked firmware package does not detect as {}",
                model.name()
            ));
        }
        let browser_model = patch_wasm::detect_firmware_model(&stock)
            .map_err(|e| format!("WASM firmware model detection failed: {e:?}"))?;
        if browser_model != model.label() {
            return Err(format!(
                "WASM detected {browser_model}, expected {}",
                model.label()
            ));
        }
        Ok(Self {
            model,
            firmware: options.firmware_dir.clone(),
            root,
            stock,
            profiles: vec![],
            loader_profiles: vec![],
        })
    }
    fn firmware(&mut self, patches: &[String]) -> Result<(), String> {
        let out = self.root.join(format!("native-{}", self.profiles.len()));
        let native = native(&self.firmware, &out, patches, l6patch::BuildMode::Firmware)
            .map_err(|e| format!("native patch build failed for {patches:?}: {e}"))?;
        let selected: Vec<_> = patches.iter().map(String::as_str).collect();
        let (browser, report) = patch_wasm::apply_patches(&self.stock, &selected)
            .map_err(|e| format!("WASM patching failed for {patches:?}: {e}"))?;
        compare_bytes(self.model, patches, &browser, &native)?;
        compare_callback_metadata(&out, &report)?;
        self.profiles.push(evidence(patches, &browser, &report));
        Ok(())
    }
    fn loader(&mut self, patches: &[String]) -> Result<(), String> {
        let mask = patch_core::WASM_PATCHES
            .iter()
            .enumerate()
            .filter(|(_, p)| patches.iter().any(|s| s == **p))
            .fold(0usize, |mask, (i, _)| mask | (1 << i));
        let out = self.root.join(format!("native-loader-{mask:03}"));
        let native = native(
            &self.firmware,
            &out,
            patches,
            l6patch::BuildMode::PatchLoader,
        );
        let selected: Vec<_> = patches.iter().map(String::as_str).collect();
        let browser = patch_wasm::apply_patches_with_loader(&self.stock, &selected, true);
        let native = match native {
            Ok(bytes) => bytes,
            Err(n) if n.contains("installed patch payloads overlap the SD module region") => {
                return validate_overlap_rejection(patches, &n, browser);
            }
            Err(error) => {
                return Err(format!(
                    "native patch-loader build failed for {patches:?}: {error}"
                ));
            }
        };
        let (browser, report) =
            browser.map_err(|e| format!("WASM patch-loader build failed for {patches:?}: {e}"))?;
        if browser != native {
            fs::write(
                self.root.join(format!("wasm-loader-{mask:03}.bin")),
                &browser,
            )
            .map_err(|e| e.to_string())?;
            let differences = browser
                .iter()
                .zip(&native)
                .enumerate()
                .filter_map(|(i, (a, b))| (a != b).then_some((i, *a, *b)))
                .take(16)
                .collect::<Vec<_>>();
            return Err(format!(
                "L6max loader {patches:?} mismatch at {differences:?}: WASM {} bytes SHA-256 {}, native {} bytes SHA-256 {}",
                browser.len(),
                digest(&browser),
                native.len(),
                digest(&native)
            ));
        }
        compare_callback_metadata(&out, &report)?;
        self.loader_profiles
            .push(evidence(patches, &browser, &report));
        Ok(())
    }
    fn unsupported(&self, patch: &str) -> Result<(), String> {
        match patch_wasm::apply_patches(&self.stock, &[patch]) {
            Err(message) if message.contains("is not available for L6 in the browser") => {}
            Err(message) => {
                return Err(format!(
                    "WASM rejected unsupported L6 profile {patch} for an unexpected reason: {message}"
                ));
            }
            Ok(_) => return Err(format!("WASM accepted unsupported L6 profile {patch}")),
        }
        match native(
            &self.firmware,
            &self.root.join(format!("native-unsupported-{patch}")),
            &[patch.into()],
            l6patch::BuildMode::Firmware,
        ) {
            Err(message) if message.contains("is not available for L6 firmware") => Ok(()),
            Err(_) => Err(format!(
                "native patcher rejected unsupported L6 profile {patch} for an unexpected reason"
            )),
            Ok(_) => Err(format!(
                "native patcher accepted unsupported L6 profile {patch}"
            )),
        }
    }
    fn finish(&self, logs: &Path) -> Result<(), String> {
        let report = json!({"model":self.model.name(),"stock_sha256":digest(&self.stock),"profiles":self.profiles,"patch_loader_profiles":self.loader_profiles});
        fs::write(
            logs.join("wasm-patch.json"),
            serde_json::to_vec_pretty(&report).map_err(|e| e.to_string())?,
        )
        .map_err(|e| e.to_string())
    }
}

pub(crate) fn run(scenario: Scenario, options: Options) -> Result<(), String> {
    let main =
        fs::read(options.firmware_dir.join("main_firmware.bin")).map_err(|e| e.to_string())?;
    let model = l6fw::model_from_main(&main).map_err(|e| e.to_string())?;
    let steps: Vec<_> = scenario
        .step
        .iter()
        .filter(|step| step.applies_to(model))
        .collect();
    let progress = CaseProgress::from_env();
    progress.emit(CaseEvent::ScenarioStarted {
        total_steps: steps.len(),
    });
    let start = Instant::now();
    let mut parity = None;
    let mut completed = Vec::new();
    let mut failure = None;
    for (index, step) in steps.iter().enumerate() {
        progress.emit(CaseEvent::StepStarted {
            index: index + 1,
            label: step_label(&step.action),
            started_unix_ms: unix_ms(),
        });
        let result = match &step.action {
            Action::PatchParityPrepare => Parity::prepare(&options).map(|value| {
                parity = Some(value);
            }),
            Action::PatchParityFirmware { patches } => parity
                .as_mut()
                .ok_or_else(|| "missing patch parity preparation".to_string())
                .and_then(|p| p.firmware(patches)),
            Action::PatchParityLoader { patches } => parity
                .as_mut()
                .ok_or_else(|| "missing patch parity preparation".to_string())
                .and_then(|p| p.loader(patches)),
            Action::PatchParityUnsupported { patch } => parity
                .as_ref()
                .ok_or_else(|| "missing patch parity preparation".to_string())
                .and_then(|p| p.unsupported(patch)),
            Action::PatchParityFinish => parity
                .as_ref()
                .ok_or_else(|| "missing patch parity preparation".to_string())
                .and_then(|p| p.finish(&options.logs)),
            _ => Err("invalid patch parity step".into()),
        };
        match result {
            Ok(()) => completed.push(json!({"step":index + 1,"device":true})),
            Err(error) => {
                failure = Some((index + 1, error.to_string()));
                break;
            }
        }
    }
    let report = json!({"schema":1,"scenario":scenario.name,"model":model.name(),"outcome":if failure.is_none(){"PASS"}else{"FAIL"},"failed_step":failure.as_ref().map(|f|f.0),"failed_step_label":failure.as_ref().map(|f|step_label(&steps[f.0-1].action)),"error":failure.as_ref().map(|f|&f.1),"wall_seconds":start.elapsed().as_secs_f64(),"completed_steps":completed});
    fs::write(
        options.logs.join("scenario.json"),
        serde_json::to_vec_pretty(&report).map_err(|e| e.to_string())?,
    )
    .map_err(|e| e.to_string())?;
    progress.emit(CaseEvent::ScenarioFinished {
        failure: failure
            .as_ref()
            .map(|(index, error)| smoke_core::progress::StepFailure {
                index: *index,
                label: step_label(&steps[*index - 1].action),
                error: error.clone(),
            }),
        finished_unix_ms: unix_ms(),
        outcome: if failure.is_none() {
            Outcome::Pass
        } else {
            Outcome::Fail
        },
    });
    failure.map_or(Ok(()), |(_, error)| Err(error))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn knob_metadata_requires_one_export_only_when_knob_is_present() {
        assert_eq!(knob_callback(&json!({"patches":[]})).unwrap(), None);
        assert_eq!(
            knob_callback(&json!({"patches":[{"name":"knob-dialog","knob_callback":42}]})).unwrap(),
            Some(42)
        );
        assert!(knob_callback(&json!({"patches":[{"name":"knob-dialog"}]})).is_err());
        assert!(knob_callback(&json!({"patches":[{"name":"knob-dialog","knob_callback":42},{"name":"knob-dialog","knob_callback":42}]})).is_err());
    }

    #[test]
    fn loader_rejection_requires_matching_overlap_class() {
        let profiles = vec!["knob-dialog".into()];
        assert!(
            validate_overlap_rejection(
                &profiles,
                "native overlap",
                Err("overlap the Patch loader module reserve".into())
            )
            .is_ok()
        );
        assert!(
            validate_overlap_rejection(
                &profiles,
                "native overlap",
                Err("different failure".into())
            )
            .is_err()
        );
        assert!(
            validate_overlap_rejection(&profiles, "native overlap", Ok((vec![], Value::Null)))
                .is_err()
        );
    }

    #[test]
    fn mismatch_checks_full_bytes_including_length_and_reports_hashes() {
        assert!(compare_bytes(l6fw::Model::L6, &["midi-sync".into()], b"same", b"same").is_ok());
        let error = compare_bytes(
            l6fw::Model::L6,
            &["midi-sync".into()],
            b"same",
            b"same-extra",
        )
        .unwrap_err();
        assert!(error.contains("midi-sync") && error.contains("SHA-256"));
        assert!(compare_bytes(l6fw::Model::L6max, &[], b"a", b"b").is_err());
    }
}
