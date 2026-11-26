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
    use super::*;
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
