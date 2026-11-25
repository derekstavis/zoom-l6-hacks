//! Running one case: preflight, fixture, invocation, identity and
//! execution. Each step classifies its own errors: a case that did not run (or
//! whose result is not valid) is BLOCKED; a fixture or execution failure is
//! FAIL.
use crate::{
    cache::RunCache,
    catalog::{Case, ExpectedFailure},
    config::Config,
    fixture::{
        Execution, Fixture, Inputs, engine_args, expand, fixture_options, package_model,
        prepare_sd_patch, wasm_patch,
    },
    hashing::{digest, files, hash, hash_sparse},
    identity::CaseIdentity,
    report::{CaseError, CaseResult, InPhase, Outcome, Phase, Stage, StageRecorder},
    supervise::{cancelled, supervise},
};
use l6fw::Model;
use l6max_host::engine::Options;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use smoke_core::{Backend, Scenario, fixture_layout as layout};
use std::{
    collections::BTreeMap,
    env, fs,
    path::{Path, PathBuf},
    process::Command,
    time::Instant,
};

fn check_l6_patched_main(main: &[u8], stock_sha256: &str) -> Result<(), String> {
    if format!("{:x}", Sha256::digest(main)) == stock_sha256 {
        return Err("patched L6 update MAIN must differ from stock".into());
    }
    Ok(())
}

/// The same synthetic bad-checksum package used by the legacy L6max lifecycle.
fn synthetic_update(
    mut package: Vec<u8>,
    fixture: smoke_core::UpdateFixture,
) -> Result<Vec<u8>, String> {
    l6fw::update::validate(&package).map_err(|error| error.to_string())?;
    for offset in [0x1a31fc, 0x1a81fc] {
        package[offset..offset + 4].copy_from_slice(&l6fw::PATCHED_VERSION);
    }
    package[0x1a71fc..0x1a7200].copy_from_slice(b"0101");
    package[0x21c..0x220].copy_from_slice(&0x13579bdfu32.to_le_bytes());
    package[0x1a321c..0x1a3220].copy_from_slice(&0x2468ace0u32.to_le_bytes());
    match fixture {
        smoke_core::UpdateFixture::BadChecksum => {
            if !l6fw::update::validate(&package)
                .is_err_and(|error| error.to_string().contains("checksum"))
            {
                return Err("synthetic update did not have the intended checksum failure".into());
            }
        }
        smoke_core::UpdateFixture::PatchedMidi
        | smoke_core::UpdateFixture::BadChecksumStock
        | smoke_core::UpdateFixture::L6Stock
        | smoke_core::UpdateFixture::L6PatchedMidi => {
            return Err("update is not synthetic".into());
        }
        smoke_core::UpdateFixture::SyntheticValid => {
            l6fw::update::recompute_checksum(&mut package).map_err(|error| error.to_string())?;
            l6fw::update::validate(&package).map_err(|error| error.to_string())?;
        }
    }
    Ok(package)
}

fn checked_update(
    source_bytes: Vec<u8>,
    recorded: Option<&str>,
    fixture: smoke_core::UpdateFixture,
) -> Result<Vec<u8>, String> {
    let source_hash = format!("{:x}", Sha256::digest(&source_bytes));
    if source_hash != smoke_core::firmware::STOCK_L6MAX_PACKAGE_SHA256
        || recorded != Some(source_hash.as_str())
    {
        return Err("bad-checksum update needs the hash-pinned L6max 1.10 package matching its staged identity".into());
    }
    if fixture == smoke_core::UpdateFixture::BadChecksumStock {
        l6fw::update::validate(&source_bytes).map_err(|error| error.to_string())?;
        let mut corrupt = source_bytes;
        corrupt[0x1fc] ^= 1;
        if !l6fw::update::validate(&corrupt)
            .is_err_and(|error| error.to_string().contains("checksum"))
        {
            return Err("stock update did not have the intended checksum failure".into());
        }
        Ok(corrupt)
    } else {
        synthetic_update(source_bytes, fixture)
    }
}

/// What every case of a run shares.
pub(crate) struct Context<'a> {
    pub(crate) root: &'a Path,
    pub(crate) output: &'a Path,
    pub(crate) config: &'a Config,
    pub(crate) cache: &'a RunCache,
    pub(crate) patch_artifact_dir: &'a Path,
    /// Stop after building fixtures and computing identities.
    pub(crate) fixtures_only: bool,
    /// Let display checks replace references that differ.
    pub(crate) update_references: bool,
}

/// How an attempt ended without an error.
enum CaseRun {
    Ran { success: bool, scenario: Value },
    FixturesOnly,
}

/// A case's built fixture and engine options.
struct Prepared {
    scenario: Option<Scenario>,
    fixture: Fixture,
    options: Options,
    inputs: Inputs,
    spec_state: bool,
}

/// The command a case runs, and what it was built from.
struct Invocation {
    command: Command,
    binary: PathBuf,
    binary_sha256: String,
    arguments: Vec<String>,
    environment: BTreeMap<String, String>,
    audio_input: Vec<(u8, String)>,
}

/// Run one attempt of a case in `dir`.
pub(crate) fn run_case(
    ctx: &Context,
    fixture: &Fixture,
    case: &Case,
    backend: Backend,
    dir: &Path,
    midi_namespace: &str,
    progress_id: &smoke_core::progress::AttemptId,
    progress: &crate::progress::Journal,
) -> CaseResult {
    let started = Instant::now();
    let mut result = CaseResult::new(&case.id, fixture.model, backend, dir);
    let stages = StageRecorder::default();
    match attempt(
        ctx,
        fixture,
        case,
        backend,
        dir,
        midi_namespace,
        progress_id,
        progress,
        &mut result,
        &stages,
    ) {
        Ok(CaseRun::Ran { success, scenario }) => {
            result.phase = Phase::Execute;
            result.outcome =
                if success && (case.scenario.is_none() || scenario["outcome"] == "PASS") {
                    Outcome::Pass
                } else {
                    Outcome::Fail
                };
            result.detail = scenario["error"]
                .as_str()
                .unwrap_or(if success {
                    "completed"
                } else {
                    "child failed; inspect stdout.log and stderr.log"
                })
                .into();
            apply_expected(&mut result, &scenario, &case.expected_failure);
        }
        Ok(CaseRun::FixturesOnly) => {
            result.outcome = Outcome::Pass;
            result.phase = Phase::Fixture;
            result.detail = "fixtures built; not run".into();
        }
        Err(error) => {
            result.outcome = error.outcome;
            result.phase = error.phase;
            result.detail = error.message;
        }
    }
    result.duration_s = started.elapsed().as_secs_f64();
    result.timings.stages = stages.results();
    let _ = fs::create_dir_all(dir);
    let _ = fs::write(
        dir.join("result.json"),
        serde_json::to_vec_pretty(&result).unwrap(),
    );
    result
}

fn attempt(
    ctx: &Context,
    fixture: &Fixture,
    case: &Case,
    backend: Backend,
    dir: &Path,
    midi_namespace: &str,
    progress_id: &smoke_core::progress::AttemptId,
    progress: &crate::progress::Journal,
    result: &mut CaseResult,
    stages: &StageRecorder,
) -> Result<CaseRun, CaseError> {
    let timer = stages.start(Stage::Preflight);
    let scenario = preflight(ctx, fixture, case, backend)?;
    drop(timer);
    let started = Instant::now();
    let built = (|| {
        let prepared = build_fixture(ctx, fixture, case, scenario, dir, stages)?;
        let _timer = stages.start(Stage::Invocation);
        let invocation = plan_invocation(
            ctx,
            case,
            backend,
            dir,
            midi_namespace,
            &prepared,
            progress_id,
            stages,
        )
        .failed_in(Phase::Fixture)?;
        Ok::<_, CaseError>((prepared, invocation))
    })();
    result.timings.fixture_s = started.elapsed().as_secs_f64();
    let (prepared, invocation) = built?;
    let timer = stages.start(Stage::Identity);
    result.identity = identify(ctx, &prepared, &invocation).map(|identity| json!(identity));
    drop(timer);
    if ctx.fixtures_only {
        return Ok(CaseRun::FixturesOnly);
    }
    progress.attempt_phase(progress_id.clone(), smoke_core::progress::Phase::Executing);
    execute(ctx, case, dir, invocation, result, stages)
}

/// Whether the case can run here at all. Problems are BLOCKED.
fn preflight(
    ctx: &Context,
    fixture: &Fixture,
    case: &Case,
    backend: Backend,
) -> Result<Option<Scenario>, CaseError> {
    let blocked = |message: String| Err(CaseError::blocked(Phase::Preflight, message));
    if cancelled() {
        return blocked("not run: suite interrupted".into());
    }
    if let Some(reason) = &case.blocked {
        return blocked(reason.clone());
    }
    let scenario = case
        .scenario
        .as_ref()
        .map(|path| Scenario::load(&crate::catalog::scenario_file(path)))
        .transpose()
        .failed_in(Phase::Preflight)?;
    if let Some(scenario) = &scenario
        && !scenario.backends.contains(&backend)
    {
        return blocked(format!("scenario does not support {backend} backend"));
    }
    if case.scenario.is_none() && ctx.config.app.is_none() {
        return blocked("requires a packaged application; pass --app PATH".into());
    }
    if hash(&ctx.config.qemu).blocked_in(Phase::Preflight)? != ctx.config.qemu_sha256 {
        return blocked("QEMU changed since preflight".into());
    }
    if let Some(package) = &fixture.firmware_package {
        let model = package_model(ctx.cache, package).blocked_in(Phase::Preflight)?;
        if model != fixture.model {
            return blocked(format!(
                "configured {} firmware is a {model} package",
                fixture.model
            ));
        }
    }
    Ok(scenario)
}

/// Build the case's firmware, SD bundle, update package, state and card.
fn build_fixture(
    ctx: &Context,
    fixture: &Fixture,
    case: &Case,
    scenario: Option<Scenario>,
    dir: &Path,
    stages: &StageRecorder,
) -> Result<Prepared, CaseError> {
    let failed = |message: String| CaseError::failed(Phase::Fixture, message);
    let mut selected = fixture.clone();
    let timer = stages.start(Stage::SdPatch);
    prepare_sd_patch(&mut selected, dir, ctx.cache, stages).failed_in(Phase::Fixture)?;
    drop(timer);
    let timer = stages.start(Stage::UpdatePackage);
    let update_patches = scenario
        .as_ref()
        .and_then(|scenario| scenario.update_patches.get(&fixture.model).cloned())
        .unwrap_or_default();
    if !update_patches.is_empty() {
        let package = fixture.firmware_package.as_ref().ok_or_else(|| {
            CaseError::blocked(Phase::Preflight, "update_patches require firmware")
        })?;
        let (cached, report) =
            wasm_patch(ctx.cache, package, &update_patches).failed_in(Phase::Fixture)?;
        fs::create_dir_all(dir).failed_in(Phase::Fixture)?;
        let update = dir.join(layout::PATCHED_UPDATE);
        fs::copy(&cached, &update).failed_in(Phase::Fixture)?;
        // The post-update checks locate the installed hooks from the report
        // that sits beside the package.
        fs::write(
            update.with_extension("json"),
            serde_json::to_vec_pretty(&report).unwrap(),
        )
        .failed_in(Phase::Fixture)?;
        selected.update_package = Some(update);
    }
    if case.no_sd {
        selected.card_spec = None;
    }
    drop(timer);
    let (mut options, mut inputs) =
        fixture_options(ctx.config, &selected, dir, ctx.cache, stages).failed_in(Phase::Fixture)?;
    if let Some(fixture) = scenario.as_ref().and_then(|s| s.update_fixture) {
        if case.no_sd
            || case.volatile
            || selected.state_spec.is_some()
            || selected.card_spec.is_some()
        {
            return Err(failed(
                "managed update needs an isolated persistent state and its own card".into(),
            ));
        }
        let expected_model = if fixture.is_l6() {
            Model::L6
        } else {
            Model::L6max
        };
        if inputs.firmware.get("main_firmware.bin").map(String::as_str)
            != Some(expected_model.stock_main_sha256())
        {
            return Err(failed(
                "managed update needs the model’s hash-pinned stock MAIN".into(),
            ));
        }
        let source = selected
            .firmware_package
            .as_ref()
            .ok_or_else(|| failed("update card needs a firmware package".into()))?;
        let source_bytes = fs::read(source).failed_in(Phase::Fixture)?;
        let package = if fixture == smoke_core::UpdateFixture::L6Stock {
            let hash = format!("{:x}", Sha256::digest(&source_bytes));
            if hash != smoke_core::firmware::STOCK_L6_PACKAGE_SHA256
                || inputs.package_sha256.as_deref() != Some(hash.as_str())
            {
                return Err(failed("L6 update requires the pinned stock package".into()));
            }
            if l6fw::model_from_package(&source_bytes).map_err(|e| failed(e.to_string()))?
                != Model::L6
            {
                return Err(failed("L6 update package model mismatch".into()));
            }
            source_bytes
        } else if fixture.is_patched() {
            let stock_hash = format!("{:x}", Sha256::digest(&source_bytes));
            if stock_hash
                != if fixture.is_l6() {
                    smoke_core::firmware::STOCK_L6_PACKAGE_SHA256
                } else {
                    smoke_core::firmware::STOCK_L6MAX_PACKAGE_SHA256
                }
                || inputs.package_sha256.as_deref() != Some(stock_hash.as_str())
            {
                return Err(failed(
                    "patched update requires the pinned stock package".into(),
                ));
            }
            let patched = selected
                .update_package
                .as_ref()
                .ok_or_else(|| failed("missing patched update".into()))?;
            let bytes = fs::read(patched).failed_in(Phase::Fixture)?;
            if fixture.is_l6() {
                if l6fw::model_from_package(&bytes).map_err(|e| failed(e.to_string()))? != Model::L6
                {
                    return Err(failed("patched L6 update package model mismatch".into()));
                }
                l6fw::unpack_bytes(&bytes).map_err(|e| failed(e.to_string()))?;
            } else {
                l6fw::update::validate(&bytes).map_err(|e| failed(e.to_string()))?;
                l6max_host::package::check_layout(&bytes).map_err(|e| failed(e.to_string()))?;
                l6max_host::update_bootloader::validate(&bytes)
                    .map_err(|e| failed(e.to_string()))?;
                if bytes.get(0x1a31fc..0x1a3200) != Some(l6fw::PATCHED_VERSION.as_slice()) {
                    return Err(failed(
                        "patched L6max update must carry the patched version".into(),
                    ));
                }
            }
            if inputs.update_package_sha256.as_deref()
                != Some(format!("{:x}", Sha256::digest(&bytes)).as_str())
            {
                return Err(failed(
                    "patched update differs from its recorded identity".into(),
                ));
            }
            bytes
        } else {
            checked_update(source_bytes, inputs.package_sha256.as_deref(), fixture)
                .map_err(failed)?
        };
        let update = dir.join("staged-update.bin");
        fs::write(&update, &package).failed_in(Phase::Fixture)?;
        if fixture.is_patched() {
            let report = dir.join(layout::PATCHED_UPDATE.replace(".bin", ".json"));
            let report_bytes = fs::read(&report).failed_in(Phase::Fixture)?;
            let report_json: Value =
                serde_json::from_slice(&report_bytes).failed_in(Phase::Fixture)?;
            let package_hash = format!("{:x}", Sha256::digest(&package));
            let patch_names = report_json["patches"]
                .as_array()
                .ok_or_else(|| failed("update report lacks patches".into()))?;
            let plan = patch_core::Plan::new(
                expected_model,
                patch_core::Delivery::Browser,
                &update_patches,
                &[],
            )
            .map_err(failed)?;
            let reported: Vec<_> = patch_names
                .iter()
                .filter_map(|patch| patch["name"].as_str())
                .filter(|name| *name != "patch-loader")
                .collect();
            if report_json["model"].as_str() != Some(expected_model.label())
                || report_json["update_sha256"].as_str() != Some(package_hash.as_str())
                || reported != plan.patches.iter().map(String::as_str).collect::<Vec<_>>()
            {
                return Err(failed(
                    "update report does not identify requested patches and package".into(),
                ));
            }
            let verify = dir.join("verify-firmware");
            let unpacked = crate::fixture::unpacked(ctx.cache, &update).map_err(failed)?;
            crate::cache::copy_tree(&unpacked, &verify).map_err(failed)?;
            fs::write(verify.join(layout::FIRMWARE_REPORT), &report_bytes)
                .failed_in(Phase::Fixture)?;
            fs::write(verify.join(layout::PATCH_REPORT), &report_bytes)
                .failed_in(Phase::Fixture)?;
            let main = fs::read(verify.join("main_firmware.bin")).failed_in(Phase::Fixture)?;
            if fixture.is_l6() {
                check_l6_patched_main(&main, Model::L6.stock_main_sha256()).map_err(failed)?;
            }
            if !matches!(
                smoke_core::firmware::identify(&main, &verify),
                smoke_core::firmware::FirmwareIdentity::Built { .. }
            ) {
                return Err(failed(
                    "update report does not identify unpacked patched MAIN".into(),
                ));
            }
            inputs.update_report_sha256 = Some(format!("{:x}", Sha256::digest(&report_bytes)));
            inputs.verify_firmware = Some(files(&verify).map_err(failed)?);
        }
        let card = dir.join("staged-update-card.img");
        let filename = if fixture.is_l6() {
            "L6.BIN"
        } else {
            "L6max.BIN"
        };
        if fixture.is_l6() {
            sd_image::create(&card, &update).map_err(|e| failed(e.to_string()))?;
        } else {
            let mut volume =
                sd_image::create_empty(&card, "L6MAX").map_err(|e| failed(e.to_string()))?;
            volume
                .write(filename, &package)
                .and_then(|()| volume.flush())
                .map_err(|e| failed(e.to_string()))?;
            drop(volume);
        }
        if sd_image::read_file(&card, filename, package.len() as u64 + 1)
            .map_err(|error| failed(error.to_string()))?
            != package
        {
            return Err(failed(
                "update card content differs from staged package".into(),
            ));
        }
        inputs.update_card_package_sha256 = Some(hash(&update).failed_in(Phase::Fixture)?);
        inputs.sd_sha256 = Some(hash_sparse(&card).failed_in(Phase::Fixture)?);
        options.sd_image = Some(card);
        options.no_sd = false;
    }
    let timer = stages.start(Stage::ScenarioPreparation);
    if case.volatile && selected.state_spec.is_none() {
        options.state_dir = None;
    }
    if case.no_sd {
        options.sd_image = None;
        options.no_sd = true;
    }
    // Setup-input steps are skipped when the spec already built their result.
    let spec_state = selected.state_spec.is_some();
    if let Some(scenario) = &scenario {
        for file in layout::required(scenario, fixture.model) {
            if !dir.join(&file).exists() {
                return Err(failed(format!(
                    "the fixture lacks {file}, which the scenario reads"
                )));
            }
        }
        let runnable = if spec_state {
            scenario
                .clone()
                .without_setup_input()
                .map_err(|error| failed(format!("invalid scenario after setup pruning: {error}")))?
        } else {
            scenario.clone()
        };
        // Firmware guards and other requirements. On the configured stock
        // image an unmet guard means the case cannot run here (BLOCKED); on an
        // image our tools built, a mismatch is a regression in that build.
        let derived = !fixture.patches.is_empty()
            || fixture.sd_patches.is_some()
            || !update_patches.is_empty();
        smoke_core::devices::prepare(&runnable, &mut options).map_err(|message| {
            if derived {
                CaseError::failed(Phase::Fixture, message)
            } else {
                CaseError::blocked(Phase::Preflight, message)
            }
        })?;
    }
    drop(timer);
    let timer = stages.start(Stage::ExecutionInputs);
    (|| -> Result<(), String> {
        inputs.execution = Some(Execution {
            spec_state,
            volatile: case.volatile,
            no_sd: case.no_sd,
            sd_sha256: options.sd_image.as_deref().map(hash_sparse).transpose()?,
            state_sha256: options.state_dir.as_deref().map(files).transpose()?,
        });
        fs::write(
            dir.join("inputs.json"),
            serde_json::to_vec_pretty(&inputs).unwrap(),
        )
        .map_err(|e| e.to_string())
    })()
    .failed_in(Phase::Fixture)?;
    drop(timer);
    Ok(Prepared {
        scenario,
        fixture: selected,
        options,
        inputs,
        spec_state,
    })
}

/// The executable, arguments and environment the case runs with.
fn plan_invocation(
    ctx: &Context,
    case: &Case,
    backend: Backend,
    dir: &Path,
    midi_namespace: &str,
    prepared: &Prepared,
    progress_id: &smoke_core::progress::AttemptId,
    stages: &StageRecorder,
) -> Result<Invocation, String> {
    let config = ctx.config;
    let options = &prepared.options;
    // A case without a scenario drives the packaged application itself.
    let binary = match (&case.scenario, backend) {
        (None, _) => config
            .app
            .as_ref()
            .ok_or("requires a packaged application; pass --app PATH")?
            .executable
            .clone(),
        (Some(_), Backend::Headless) => ctx.output.join("bin/l6test"),
        (Some(_), _) => ctx.output.join("bin/l6max-gui"),
    };
    let timer = stages.start(Stage::InvocationBinaryHash);
    let binary_sha256 = hash(&binary)?;
    drop(timer);
    let mut command = Command::new(&binary);
    command.current_dir(ctx.root);
    for (key, _) in env::vars_os() {
        if key.to_string_lossy().starts_with("L6_") || key.to_string_lossy().starts_with("L6MAX_") {
            command.env_remove(key);
        }
    }
    let log_format = env::var("L6_LOG_FORMAT").unwrap_or_else(|_| "json".into());
    command.env("L6_LOG_FORMAT", log_format);
    command
        .env("SMOKE_CASE", &case.id)
        .env("SMOKE_MODEL", prepared.fixture.model.name())
        .env("SMOKE_BACKEND", backend.name())
        .env(
            smoke_core::STATE_SPEC_ENV,
            if prepared.spec_state { "1" } else { "0" },
        )
        .env("L6_MIDI_NAMESPACE", midi_namespace);
    command.env("L6_PATCH_ARTIFACT_DIR", ctx.patch_artifact_dir);
    if case.scenario.is_some() {
        command.env(smoke_core::progress::PATH_ENV, dir.join("progress.jsonl"));
        command.env(
            smoke_core::progress::RUN_ENV,
            ctx.output.display().to_string(),
        );
        command.env(
            smoke_core::progress::ATTEMPT_ENV,
            serde_json::to_string(progress_id).map_err(|e| e.to_string())?,
        );
    }
    if ctx.update_references {
        command
            .env(smoke_core::devices::UPDATE_REFERENCES_ENV, "1")
            .env(
                smoke_core::devices::REFERENCE_RECORDS_ENV,
                ctx.output.join("references"),
            );
    }
    command.env(
        "RUST_LOG",
        env::var("RUST_LOG").unwrap_or_else(|_| l6max_host::logging::DEFAULT_FILTER.into()),
    );
    if let Some(path) = &case.scenario {
        let scenario = crate::catalog::scenario_file(path);
        if backend == Backend::Headless {
            command.arg("__scenario").arg(&scenario);
        } else {
            command.arg("--smoke-scenario").arg(&scenario);
        }
    }
    command.args(engine_args(options));
    let timer = stages.start(Stage::AudioInputPreparation);
    let mut audio_input = Vec::new();
    if !prepared.fixture.audio_input.is_empty() {
        let directory = dir.join("audio-input");
        for input in &prepared.fixture.audio_input {
            let bytes = input
                .content
                .bytes(&l6max_host::paths::repository_root())
                .map_err(|e| e.to_string())?;
            audio_input.push((input.channel, format!("{:x}", Sha256::digest(&bytes))));
            l6max_host::audio_input::prepare_bytes(&directory, u32::from(input.channel), &bytes)
                .map_err(|e| e.to_string())?;
        }
        command.env("L6_AUDIO_INPUT_DIR", directory);
    }
    drop(timer);
    for (key, value) in &case.environment {
        let mut value = expand(value, options);
        if let Some(package) = config.firmware.get(&Model::L6) {
            value = value.replace("{l6_package}", &package.display().to_string());
        }
        if let Some(app) = &config.app {
            value = value.replace("{app_qemu_sha256}", &app.qemu_sha256);
        }
        if value.contains('{') {
            return Err(format!("unresolved placeholder in {key}: {value}"));
        }
        command.env(key, value);
    }
    let arguments = command
        .get_args()
        .map(|s| s.to_string_lossy().into_owned())
        .collect();
    let environment = command
        .get_envs()
        .filter_map(|(k, v)| {
            v.map(|v| {
                (
                    k.to_string_lossy().into_owned(),
                    v.to_string_lossy().into_owned(),
                )
            })
        })
        .collect();
    Ok(Invocation {
        command,
        binary,
        binary_sha256,
        arguments,
        environment,
        audio_input,
    })
}

/// The binaries and fixtures the case runs on; none for a case without a
/// scenario.
fn identify(ctx: &Context, prepared: &Prepared, invocation: &Invocation) -> Option<CaseIdentity> {
    prepared.scenario.as_ref()?;
    let inputs = &prepared.inputs;
    let execution = inputs.execution.as_ref()?;
    Some(CaseIdentity {
        qemu_sha256: ctx.config.qemu_sha256.clone(),
        runner_sha256: invocation.binary_sha256.clone(),
        firmware_package_sha256: inputs.package_sha256.clone(),
        update_package_sha256: inputs.update_package_sha256.clone(),
        update_card_package_sha256: inputs.update_card_package_sha256.clone(),
        update_report_sha256: inputs.update_report_sha256.clone(),
        verify_firmware: inputs
            .verify_firmware
            .as_ref()
            .map(|files| digest(&json!(files))),
        firmware: digest(&json!(inputs.firmware)),
        card_sha256: execution.sd_sha256.clone(),
        state: digest(&json!(execution.state_sha256)),
        audio_input: invocation.audio_input.clone(),
    })
}

/// Run the case's child process under supervision.
fn execute(
    ctx: &Context,
    case: &Case,
    dir: &Path,
    invocation: Invocation,
    result: &mut CaseResult,
    stages: &StageRecorder,
) -> Result<CaseRun, CaseError> {
    let timer = stages.start(Stage::ExecutionSetup);
    let Invocation {
        command,
        binary,
        binary_sha256,
        arguments,
        environment,
        ..
    } = invocation;
    let record = json!({"binary":binary,"binary_sha256":binary_sha256,"arguments":arguments,"environment":environment});
    fs::write(
        dir.join("invocation.json"),
        serde_json::to_vec_pretty(&record).unwrap(),
    )
    .failed_in(Phase::Execute)?;
    drop(timer);
    let started = Instant::now();
    let success = supervise(command, dir, case.wall_timeout_s);
    result.timings.execute_s = started.elapsed().as_secs_f64();
    if let Some(seconds) = crate::report::child_shutdown_seconds(
        &dir.join("progress.jsonl"),
        smoke_core::progress::unix_ms(),
    ) {
        stages.record(Stage::ChildShutdown, seconds);
    }
    let success = success.map_err(|message| {
        if cancelled() {
            // An interrupted case did not complete; it is not a failure.
            CaseError::blocked(Phase::Execute, message)
        } else {
            CaseError::failed(Phase::Execute, message)
        }
    })?;
    // A changed executable invalidates the result rather than failing it.
    let timer = stages.start(Stage::ExecutableVerification);
    if hash(&binary).blocked_in(Phase::Execute)? != binary_sha256 {
        return Err(CaseError::blocked(
            Phase::Execute,
            "the executable changed during the case; result is invalid",
        ));
    }
    if hash(&ctx.config.qemu).blocked_in(Phase::Execute)? != ctx.config.qemu_sha256 {
        return Err(CaseError::blocked(
            Phase::Execute,
            "QEMU changed during the case; result is invalid",
        ));
    }
    drop(timer);
    let scenario = fs::read(dir.join("scenario.json"))
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or(Value::Null);
    Ok(CaseRun::Ran { success, scenario })
}

/// Apply a case's expected failures for this model. A failure matching any of
/// them at its exact step and error is XFAIL; a pass despite them is XPASS.
fn apply_expected(result: &mut CaseResult, scenario: &Value, expected: &[ExpectedFailure]) {
    let for_model: Vec<_> = expected
        .iter()
        .filter(|e| e.model == result.model)
        .collect();
    let Some(first) = for_model.first() else {
        return;
    };
    if result.outcome == Outcome::Pass {
        result.outcome = Outcome::Xpass;
        result.detail = format!("unexpected pass: {}", first.reason);
    } else if result.outcome == Outcome::Fail
        && let Some(expected) = for_model.iter().find(|e| {
            scenario["failed_step"].as_u64() == Some(e.step as u64)
                && result.detail.contains(&e.contains)
        })
    {
        result.outcome = Outcome::Xfail;
        result.detail = format!("{}: {}", expected.reason, result.detail);
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn l6_patched_main_cannot_be_identical_to_stock() {
        use sha2::{Digest, Sha256};
        let stock = b"stock firmware";
        let digest = format!("{:x}", Sha256::digest(stock));
        assert!(super::check_l6_patched_main(stock, &digest).is_err());
        assert!(super::check_l6_patched_main(b"patched firmware", &digest).is_ok());
    }
    use super::*;
    #[test]
    fn bad_checksum_fixture_rejects_invalid_source() {
        assert!(synthetic_update(vec![0; 100], smoke_core::UpdateFixture::BadChecksum).is_err());
        assert!(
            checked_update(
                vec![0; 100],
                Some(smoke_core::firmware::STOCK_L6MAX_PACKAGE_SHA256),
                smoke_core::UpdateFixture::BadChecksum
            )
            .is_err()
        );
    }

    #[test]
    #[ignore = "requires L6MAX_UPDATE_TEST_PACKAGE pointing at stock 1.10"]
    fn stock_bad_checksum_fixture_flips_only_checksum_byte() {
        let original = fs::read(std::env::var_os("L6MAX_UPDATE_TEST_PACKAGE").unwrap()).unwrap();
        let fixture = smoke_core::UpdateFixture::BadChecksumStock;
        let hash = smoke_core::firmware::STOCK_L6MAX_PACKAGE_SHA256;
        assert!(checked_update(original.clone(), Some("incorrect"), fixture).is_err());
        let mut altered = original.clone();
        altered[0x300] ^= 1;
        assert!(checked_update(altered, Some(hash), fixture).is_err());
        let corrupt = checked_update(original.clone(), Some(hash), fixture).unwrap();
        let mut expected = original;
        expected[0x1fc] ^= 1;
        assert_eq!(corrupt, expected);
        assert!(
            l6fw::update::validate(&corrupt)
                .unwrap_err()
                .to_string()
                .contains("checksum")
        );
    }

    #[test]
    #[ignore = "requires L6MAX_UPDATE_TEST_PACKAGE pointing at stock 1.10"]
    fn bad_checksum_fixture_matches_legacy_mutations() {
        let path = std::env::var_os("L6MAX_UPDATE_TEST_PACKAGE").unwrap();
        let original = fs::read(path).unwrap();
        assert!(
            checked_update(
                original.clone(),
                Some("incorrect identity"),
                smoke_core::UpdateFixture::BadChecksum
            )
            .is_err()
        );
        let mut changed = original.clone();
        changed[0x300] ^= 1;
        assert!(
            checked_update(
                changed,
                Some(smoke_core::firmware::STOCK_L6MAX_PACKAGE_SHA256),
                smoke_core::UpdateFixture::BadChecksum
            )
            .is_err()
        );
        let mutated =
            synthetic_update(original.clone(), smoke_core::UpdateFixture::BadChecksum).unwrap();
        let valid =
            synthetic_update(original.clone(), smoke_core::UpdateFixture::SyntheticValid).unwrap();
        assert!(l6fw::update::validate(&valid).is_ok());
        assert_eq!(&valid[0x200..], &mutated[0x200..]);
        assert_ne!(&valid[0x1fc..0x200], &mutated[0x1fc..0x200]);
        let mut expected = original;
        for offset in [0x1a31fc, 0x1a81fc] {
            expected[offset..offset + 4].copy_from_slice(&l6fw::PATCHED_VERSION);
        }
        expected[0x1a71fc..0x1a7200].copy_from_slice(b"0101");
        expected[0x21c..0x220].copy_from_slice(&0x13579bdfu32.to_le_bytes());
        expected[0x1a321c..0x1a3220].copy_from_slice(&0x2468ace0u32.to_le_bytes());
        assert_eq!(mutated, expected);
        assert!(
            l6fw::update::validate(&mutated)
                .unwrap_err()
                .to_string()
                .contains("checksum")
        );
    }
    #[test]
    fn expected_failures_match_the_exact_step_and_error_and_expose_unexpected_passes() {
        let failures = [
            ExpectedFailure {
                model: Model::L6,
                step: 10,
                contains: "ExpectRing".into(),
                reason: "Link to the tracked investigation".into(),
            },
            // A second known failure for the same model must also match.
            ExpectedFailure {
                model: Model::L6,
                step: 12,
                contains: "ExpectLed".into(),
                reason: "Second investigation".into(),
            },
            ExpectedFailure {
                model: Model::L6max,
                step: 3,
                contains: "anything".into(),
                reason: "Other model".into(),
            },
        ];
        use Outcome::*;
        for (model, step, detail, initial, expected) in [
            ("l6", 10, "ExpectRing timed out", Fail, Xfail),
            ("l6", 12, "ExpectLed timed out", Fail, Xfail),
            ("l6", 9, "ExpectRing timed out", Fail, Fail),
            ("l6", 10, "input disconnected", Fail, Fail),
            ("l6", 12, "ExpectRing timed out", Fail, Fail),
            ("l6", 10, "completed", Pass, Xpass),
            ("l6max", 10, "ExpectRing timed out", Fail, Fail),
        ] {
            let mut result = CaseResult {
                outcome: initial,
                detail: detail.into(),
                ..CaseResult::new(
                    "controls",
                    model.parse().unwrap(),
                    Backend::Headless,
                    Path::new(""),
                )
            };
            apply_expected(&mut result, &json!({"failed_step":step}), &failures);
            assert_eq!(result.outcome, expected, "{model} step {step}: {detail}");
        }
        let mut unlisted = CaseResult {
            outcome: Pass,
            ..CaseResult::new("controls", Model::L6, Backend::Headless, Path::new(""))
        };
        apply_expected(&mut unlisted, &json!({}), &[]);
        assert_eq!(unlisted.outcome, Pass);
    }
}
