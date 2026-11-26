//! Case results, run outcome and the files that report them.
//!
//! `report.json` schema 2 keeps every schema 1 field of a result (`case`,
//! `model`, `backend`, `outcome`, `detail`, `artifacts`) and adds the phase a
//! case stopped in, durations, attempts and input identities.
use l6fw::Model;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use smoke_core::Backend;
use std::{collections::BTreeMap, fmt, io::Write, path::Path, path::PathBuf};

pub const SCHEMA: u32 = 2;

#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "UPPERCASE")]
pub enum Outcome {
    Pass,
    Fail,
    /// Failed exactly as a declared expected failure.
    Xfail,
    /// Passed although an expected failure is declared.
    Xpass,
    /// Did not run: a prerequisite, configuration or guard was not met.
    Blocked,
    /// Failed, then passed on a retry. Never counted as a pass.
    Flaky,
}

impl fmt::Display for Phase {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Phase::Preflight => "preflight",
            Phase::Fixture => "fixture",
            Phase::Execute => "execute",
        })
    }
}

impl fmt::Display for Outcome {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Outcome::Pass => "PASS",
            Outcome::Fail => "FAIL",
            Outcome::Xfail => "XFAIL",
            Outcome::Xpass => "XPASS",
            Outcome::Blocked => "BLOCKED",
            Outcome::Flaky => "FLAKY",
        })
    }
}

/// Where a case stopped. A preflight problem means the case did not run and is
/// BLOCKED; a fixture or execution problem is a FAIL.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Phase {
    Preflight,
    Fixture,
    Execute,
}

/// An error that ends a case: where it happened, and whether the case did not
/// run (BLOCKED) or failed (FAIL). A BLOCKED case is never retried.
#[derive(Debug)]
pub struct CaseError {
    pub phase: Phase,
    pub outcome: Outcome,
    pub message: String,
}

impl CaseError {
    /// The case did not run, or its result is not valid: a prerequisite or
    /// guard was not met, the run was interrupted, or an executable changed.
    pub fn blocked(phase: Phase, message: impl Into<String>) -> Self {
        Self {
            phase,
            outcome: Outcome::Blocked,
            message: message.into(),
        }
    }
    /// The case's fixture or execution failed.
    pub fn failed(phase: Phase, message: impl Into<String>) -> Self {
        Self {
            phase,
            outcome: Outcome::Fail,
            message: message.into(),
        }
    }
}

/// Classify a fallible step: `step().failed_in(Phase::Fixture)?`.
pub trait InPhase<T> {
    fn failed_in(self, phase: Phase) -> Result<T, CaseError>;
    fn blocked_in(self, phase: Phase) -> Result<T, CaseError>;
}

impl<T, E: ToString> InPhase<T> for Result<T, E> {
    fn failed_in(self, phase: Phase) -> Result<T, CaseError> {
        self.map_err(|e| CaseError::failed(phase, e.to_string()))
    }
    fn blocked_in(self, phase: Phase) -> Result<T, CaseError> {
        self.map_err(|e| CaseError::blocked(phase, e.to_string()))
    }
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct Timings {
    pub fixture_s: f64,
    pub execute_s: f64,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub stages: Vec<StageTiming>,
}

/// Host wall time spent setting up and executing the suite, measured once per run.
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct RunTimings {
    pub catalog_s: f64,
    pub generated_s: f64,
    pub preflight_s: f64,
    pub selection_s: f64,
    pub build_s: Option<f64>,
    pub staging_s: f64,
    pub cases_s: f64,
}

/// Wall-clock duration of one driver operation, including cache waits and I/O.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct StageTiming {
    pub stage: Stage,
    pub seconds: f64,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Stage {
    Preflight,
    SdPatch,
    SdStockFirmware,
    SdLoaderBuild,
    SdModuleBuild,
    UpdatePackage,
    Firmware,
    State,
    Card,
    CardHash,
    FixtureInputs,
    ScenarioPreparation,
    ExecutionInputs,
    Invocation,
    InvocationBinaryHash,
    AudioInputPreparation,
    Identity,
    ExecutionSetup,
    ChildShutdown,
    ExecutableVerification,
}

impl fmt::Display for Stage {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Preflight => "Preflight",
            Self::SdPatch => "SD patch preparation",
            Self::SdStockFirmware => "  Stock firmware for SD patch",
            Self::SdLoaderBuild => "  SD loader build",
            Self::SdModuleBuild => "  SD module build",
            Self::UpdatePackage => "Update package preparation",
            Self::Firmware => "Firmware preparation",
            Self::State => "State preparation",
            Self::Card => "Card preparation (includes card hash)",
            Self::CardHash => "Card hash",
            Self::FixtureInputs => "Fixture inputs",
            Self::ScenarioPreparation => "Scenario preparation",
            Self::ExecutionInputs => "Execution input hashes",
            Self::Invocation => "Invocation preparation",
            Self::InvocationBinaryHash => "  Invocation binary hash",
            Self::AudioInputPreparation => "  Audio input preparation",
            Self::Identity => "Identity",
            Self::ExecutionSetup => "Execution setup",
            Self::ChildShutdown => "Runner shutdown",
            Self::ExecutableVerification => "Executable verification",
        })
    }
}

/// Per-attempt recorder; a guard records even when an operation returns early.
#[derive(Clone, Default)]
pub struct StageRecorder(std::rc::Rc<std::cell::RefCell<Vec<StageTiming>>>);

impl StageRecorder {
    pub fn start(&self, stage: Stage) -> StageGuard {
        StageGuard {
            recorder: self.clone(),
            stage,
            start: std::time::Instant::now(),
        }
    }

    pub fn results(&self) -> Vec<StageTiming> {
        self.0.borrow().clone()
    }

    pub fn record(&self, stage: Stage, seconds: f64) {
        self.0.borrow_mut().push(StageTiming { stage, seconds });
    }
}

pub struct StageGuard {
    recorder: StageRecorder,
    stage: Stage,
    start: std::time::Instant,
}

impl Drop for StageGuard {
    fn drop(&mut self) {
        self.recorder.0.borrow_mut().push(StageTiming {
            stage: self.stage,
            seconds: self.start.elapsed().as_secs_f64(),
        });
    }
}

/// One execution of a case. A case without retries has one attempt.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Attempt {
    pub outcome: Outcome,
    pub phase: Phase,
    pub detail: String,
    pub artifacts: PathBuf,
    pub duration_s: f64,
    #[serde(default)]
    pub timings: Timings,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct CaseResult {
    pub case: String,
    pub model: Model,
    pub backend: Backend,
    pub outcome: Outcome,
    pub detail: String,
    pub artifacts: PathBuf,
    pub phase: Phase,
    #[serde(default)]
    pub duration_s: f64,
    #[serde(default)]
    pub timings: Timings,
    /// Hashes of the binaries and fixtures the case ran on.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub identity: Option<Value>,
    /// With `--update-references`, the reference images this case rewrote.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub updated_references: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub attempts: Vec<Attempt>,
}

impl CaseResult {
    pub fn new(case: &str, model: Model, backend: Backend, artifacts: &Path) -> Self {
        Self {
            case: case.into(),
            model,
            backend,
            outcome: Outcome::Blocked,
            detail: String::new(),
            artifacts: artifacts.into(),
            phase: Phase::Preflight,
            duration_s: 0.0,
            timings: Timings::default(),
            identity: None,
            updated_references: Vec::new(),
            attempts: Vec::new(),
        }
    }
    fn attempt(&self) -> Attempt {
        Attempt {
            outcome: self.outcome,
            phase: self.phase,
            detail: self.detail.clone(),
            artifacts: self.artifacts.clone(),
            duration_s: self.duration_s,
            timings: self.timings.clone(),
        }
    }
}

/// Run `attempt` until it stops failing in execution, at most `1 + retries`
/// times. Only execution failures are retried: a BLOCKED case did not run
/// (or was interrupted), and fixture builds are deterministic. A failure
/// followed by a pass is FLAKY; a failure followed by anything else is FAIL,
/// so a later attempt never hides an earlier unexpected failure.
pub fn with_retries(retries: usize, mut attempt: impl FnMut(usize) -> CaseResult) -> CaseResult {
    let mut attempts = Vec::new();
    let mut number = 1;
    loop {
        let result = attempt(number);
        attempts.push(result.attempt());
        let retry = result.outcome == Outcome::Fail && result.phase == Phase::Execute;
        if !retry || number > retries {
            let mut result = result;
            if let Some(failure) = attempts[..attempts.len() - 1]
                .iter()
                .find(|a| a.outcome == Outcome::Fail)
            {
                if result.outcome == Outcome::Pass {
                    result.outcome = Outcome::Flaky;
                    result.detail = format!(
                        "passed on attempt {number} after failing: {}",
                        failure.detail
                    );
                } else if result.outcome != Outcome::Fail {
                    result.detail = format!(
                        "attempt {number} was {}: {}; an earlier attempt failed: {}",
                        result.outcome, result.detail, failure.detail
                    );
                    result.outcome = Outcome::Fail;
                }
            }
            if attempts.len() > 1 {
                result.duration_s = attempts.iter().map(|a| a.duration_s).sum();
                result.attempts = attempts;
            }
            return result;
        }
        number += 1;
    }
}

/// The run's outcome and exit code. FAIL and XPASS fail the run (1); BLOCKED
/// gives 2; FLAKY fails the run only when `strict`.
pub fn summarize(results: &[CaseResult], strict: bool) -> (Outcome, i32) {
    let has = |outcome: Outcome| results.iter().any(|r| r.outcome == outcome);
    if has(Outcome::Fail) || has(Outcome::Xpass) {
        (Outcome::Fail, 1)
    } else if has(Outcome::Blocked) {
        (Outcome::Blocked, 2)
    } else if has(Outcome::Flaky) {
        (Outcome::Flaky, if strict { 1 } else { 0 })
    } else if has(Outcome::Xfail) {
        (Outcome::Xfail, 0)
    } else {
        (Outcome::Pass, 0)
    }
}

pub fn counts(results: &[CaseResult]) -> BTreeMap<String, usize> {
    let mut counts = BTreeMap::new();
    for result in results {
        *counts.entry(result.outcome.to_string()).or_default() += 1;
    }
    counts
}

pub fn report(outcome: &str, results: &[CaseResult], extra: Value) -> Value {
    let mut report = json!({
        "schema": SCHEMA,
        "outcome": outcome,
        "counts": counts(results),
        "results": results,
    });
    if let (Some(report), Value::Object(extra)) = (report.as_object_mut(), extra) {
        report.extend(extra);
    }
    report
}

/// Recover completed wall-time intervals from a case stream. An interrupted final
/// step has no end event and is intentionally omitted.
fn step_timings(path: &Path) -> Vec<(usize, String, u64)> {
    use smoke_core::progress::{CaseEvent, VERSION};
    let Ok(stream) = std::fs::read_to_string(path) else {
        return Vec::new();
    };
    let mut lines = stream.lines();
    if !matches!(
        lines
            .next()
            .and_then(|line| serde_json::from_str::<CaseEvent>(line).ok()),
        Some(CaseEvent::Header {
            version: VERSION,
            ..
        })
    ) {
        return Vec::new();
    }
    let mut current: Option<(usize, String, u64)> = None;
    let mut timings = Vec::new();
    for line in lines {
        let Ok(event) = serde_json::from_str::<CaseEvent>(line) else {
            break;
        };
        let (end, next) = match event {
            CaseEvent::StepStarted {
                index,
                label,
                started_unix_ms,
            } => (started_unix_ms, Some((index, label, started_unix_ms))),
            CaseEvent::ScenarioFinished {
                finished_unix_ms, ..
            } => (finished_unix_ms, None),
            _ => continue,
        };
        if let Some((index, label, start)) = current.take()
            && let Some(duration) = end.checked_sub(start)
        {
            timings.push((index, label, duration));
        }
        current = next;
    }
    timings.sort_by(|a, b| b.2.cmp(&a.2).then(a.0.cmp(&b.0)));
    timings
}

/// Recover completed adaptive children; incomplete events do not produce durations.
fn adaptive_child_timings(
    path: &Path,
) -> Vec<(
    usize,
    String,
    u32,
    String,
    smoke_core::progress::Outcome,
    u64,
)> {
    use smoke_core::progress::{CaseEvent, VERSION};
    let Ok(stream) = std::fs::read_to_string(path) else {
        return Vec::new();
    };
    let mut lines = stream.lines();
    if !matches!(
        lines
            .next()
            .and_then(|line| serde_json::from_str::<CaseEvent>(line).ok()),
        Some(CaseEvent::Header {
            version: VERSION,
            ..
        })
    ) {
        return Vec::new();
    }
    let mut current = None;
    let mut result = Vec::new();
    for line in lines {
        let Ok(event) = serde_json::from_str::<CaseEvent>(line) else {
            break;
        };
        match event {
            CaseEvent::AdaptiveChildStarted {
                index,
                block,
                attempt,
                child,
                started_unix_ms,
                ..
            } => current = Some((index, block, attempt, child, started_unix_ms)),
            CaseEvent::AdaptiveChildFinished {
                index,
                block,
                attempt,
                child,
                outcome,
                finished_unix_ms,
            } => {
                if let Some((i, b, a, c, start)) = current.take()
                    && i == index
                    && b == block
                    && a == attempt
                    && c == child
                    && let Some(duration) = finished_unix_ms.checked_sub(start)
                {
                    result.push((i, b, a, c, outcome, duration));
                }
            }
            CaseEvent::StepStarted { .. } | CaseEvent::ScenarioFinished { .. } => current = None,
            _ => {}
        }
    }
    result
}

fn adaptive_scope_timings(
    path: &Path,
) -> Vec<(
    usize,
    String,
    Option<u32>,
    smoke_core::progress::Outcome,
    Option<bool>,
    u64,
)> {
    use smoke_core::progress::{CaseEvent, VERSION};
    let Ok(stream) = std::fs::read_to_string(path) else {
        return Vec::new();
    };
    let mut lines = stream.lines();
    if !matches!(
        lines
            .next()
            .and_then(|line| serde_json::from_str::<CaseEvent>(line).ok()),
        Some(CaseEvent::Header {
            version: VERSION,
            ..
        })
    ) {
        return Vec::new();
    }
    let mut starts = std::collections::BTreeMap::new();
    let mut result = Vec::new();
    for line in lines {
        let Ok(event) = serde_json::from_str::<CaseEvent>(line) else {
            break;
        };
        match event {
            CaseEvent::AdaptiveScopeStarted {
                index,
                block,
                attempt,
                started_unix_ms,
                ..
            } => {
                starts.insert((index, block, attempt), started_unix_ms);
            }
            CaseEvent::AdaptiveScopeFinished {
                index,
                block,
                attempt,
                outcome,
                matched,
                finished_unix_ms,
            } => {
                if let Some(start) = starts.remove(&(index, block.clone(), attempt))
                    && let Some(duration) = finished_unix_ms.checked_sub(start)
                {
                    result.push((index, block, attempt, outcome, matched, duration));
                }
            }
            CaseEvent::StepStarted { .. } | CaseEvent::ScenarioFinished { .. } => starts.clear(),
            _ => {}
        }
    }
    result
}

/// Recover completed runner startup intervals; interrupted phases have no end.
fn case_phase_timings(path: &Path) -> Vec<(smoke_core::progress::CasePhase, u64)> {
    use smoke_core::progress::{CaseEvent, VERSION};
    let Ok(stream) = std::fs::read_to_string(path) else {
        return Vec::new();
    };
    let mut lines = stream.lines();
    if !matches!(
        lines
            .next()
            .and_then(|line| serde_json::from_str::<CaseEvent>(line).ok()),
        Some(CaseEvent::Header {
            version: VERSION,
            ..
        })
    ) {
        return Vec::new();
    }
    let mut current = None;
    let mut timings = Vec::new();
    for line in lines {
        let Ok(event) = serde_json::from_str::<CaseEvent>(line) else {
            break;
        };
        let (end, next) = match event {
            CaseEvent::PhaseChanged {
                phase,
                started_unix_ms,
            } => (started_unix_ms, Some((phase, started_unix_ms))),
            CaseEvent::StepStarted {
                started_unix_ms, ..
            } => (started_unix_ms, None),
            CaseEvent::ScenarioFinished {
                finished_unix_ms, ..
            } => (finished_unix_ms, None),
            _ => continue,
        };
        if let Some((phase, start)) = current.take()
            && let Some(duration) = end.checked_sub(start)
        {
            timings.push((phase, duration));
        }
        current = next;
    }
    timings
}

/// Time after the last scenario result until the child exits. Missing or
/// interrupted streams do not provide a reliable start time.
pub(crate) fn child_shutdown_seconds(path: &Path, exited_unix_ms: u64) -> Option<f64> {
    use smoke_core::progress::{CaseEvent, VERSION};
    let stream = std::fs::read_to_string(path).ok()?;
    let mut lines = stream.lines();
    if !matches!(
        lines
            .next()
            .and_then(|line| serde_json::from_str::<CaseEvent>(line).ok()),
        Some(CaseEvent::Header {
            version: VERSION,
            ..
        })
    ) {
        return None;
    }
    let mut finished = None;
    for line in lines {
        match serde_json::from_str::<CaseEvent>(line).ok()? {
            CaseEvent::ScenarioFinished {
                finished_unix_ms, ..
            } => finished = Some(finished_unix_ms),
            _ => {}
        }
    }
    Some(exited_unix_ms.checked_sub(finished?)? as f64 / 1000.0)
}

pub fn write_summary(
    path: &Path,
    outcome: Outcome,
    results: &[CaseResult],
    cache: &Value,
    fixtures_only: bool,
    run_timings: &RunTimings,
) -> Result<(), String> {
    let mut text = String::new();
    if fixtures_only {
        text.push_str(
            "Fixtures only: every fixture was built and identified, and no case ran.\n\n",
        );
    }
    let counts = counts(results)
        .into_iter()
        .map(|(outcome, count)| format!("{count} {outcome}"))
        .collect::<Vec<_>>()
        .join(", ");
    let total: f64 = results.iter().map(|r| r.duration_s).sum();
    text.push_str(&format!(
        "Run: {outcome}\n\n{counts}; {:.0} s of case time\n\n",
        total
    ));
    text.push_str(&format!(
        "Run wall time: catalog {:.3} s, preflight {:.3} s, selection {:.3} s, staging {:.3} s, cases {:.3} s",
        run_timings.catalog_s,
        run_timings.preflight_s,
        run_timings.selection_s,
        run_timings.staging_s,
        run_timings.cases_s
    ));
    if let Some(build) = run_timings.build_s {
        text.push_str(&format!(", build {build:.3} s"));
    }
    text.push_str(&format!(
        ", generated scenarios {:.3} s\n\n",
        run_timings.generated_s
    ));
    text.push_str("| Case | Model | Backend | Result | Phase | Seconds |\n");
    text.push_str("| --- | --- | --- | --- | --- | --- |\n");
    for result in results {
        text.push_str(&format!(
            "| {} | {} | {} | {} | {} | {:.1} |\n",
            result.case,
            result.model,
            result.backend,
            result.outcome,
            result.phase,
            result.duration_s
        ));
    }
    for result in results {
        let artifacts: Vec<&Path> = if result.attempts.is_empty() {
            vec![result.artifacts.as_path()]
        } else {
            result
                .attempts
                .iter()
                .map(|attempt| attempt.artifacts.as_path())
                .collect()
        };
        for (attempt, dir) in artifacts.iter().enumerate() {
            let timings = step_timings(&dir.join("progress.jsonl"));
            let children = adaptive_child_timings(&dir.join("progress.jsonl"));
            let scopes = adaptive_scope_timings(&dir.join("progress.jsonl"));
            let phases = case_phase_timings(&dir.join("progress.jsonl"));
            let stages = if result.attempts.is_empty() {
                &result.timings.stages
            } else {
                &result.attempts[attempt].timings.stages
            };
            if timings.is_empty()
                && stages.is_empty()
                && phases.is_empty()
                && children.is_empty()
                && scopes.is_empty()
            {
                continue;
            }
            text.push_str(&format!(
                "\n### {} · {} · {} · attempt {}\n\n",
                result.case,
                result.model,
                result.backend,
                attempt + 1
            ));
            if !stages.is_empty() {
                text.push_str("| Driver operation | Wall seconds |\n| --- | ---: |\n");
                for stage in stages {
                    text.push_str(&format!("| {} | {:.3} |\n", stage.stage, stage.seconds));
                }
                text.push('\n');
            }
            if !phases.is_empty() {
                text.push_str("| Runner phase | Wall seconds |\n| --- | ---: |\n");
                for (phase, milliseconds) in phases {
                    let label = match phase {
                        smoke_core::progress::CasePhase::PreparingScenario => "Preparing scenario",
                        smoke_core::progress::CasePhase::StartingEmulator => "Starting emulator",
                    };
                    text.push_str(&format!(
                        "| {} | {:.3} |\n",
                        label,
                        milliseconds as f64 / 1000.0
                    ));
                }
                text.push('\n');
            }
            if !timings.is_empty() {
                text.push_str("| Step | Action | Wall seconds |\n| --- | --- | ---: |\n");
            }
            for (index, label, milliseconds) in timings {
                text.push_str(&format!(
                    "| {} | {} | {:.3} |\n",
                    index,
                    label.replace('|', "\\|"),
                    milliseconds as f64 / 1000.0
                ));
            }
            if !scopes.is_empty() {
                text.push_str("\n| Step | Block | Attempt | Outcome | Matched | Wall seconds |\n| --- | --- | ---: | --- | --- | ---: |\n");
                for (index, block, attempt, outcome, matched, milliseconds) in scopes {
                    text.push_str(&format!(
                        "| {} | {} | {} | {:?} | {} | {:.3} |\n",
                        index,
                        block.replace('|', "\\|"),
                        attempt.map_or("all".into(), |n| n.to_string()),
                        outcome,
                        matched.map_or("—".into(), |value| value.to_string()),
                        milliseconds as f64 / 1000.0
                    ));
                }
            }
            if !children.is_empty() {
                text.push_str("\n| Step | Block | Attempt | Child | Outcome | Wall seconds |\n| --- | --- | ---: | --- | --- | ---: |\n");
                for (index, block, attempt, child, outcome, milliseconds) in children {
                    text.push_str(&format!(
                        "| {} | {} | {} | {} | {:?} | {:.3} |\n",
                        index,
                        block.replace('|', "\\|"),
                        attempt,
                        child.replace('|', "\\|"),
                        outcome,
                        milliseconds as f64 / 1000.0
                    ));
                }
            }
        }
    }
    let flaky: Vec<_> = results
        .iter()
        .filter(|r| r.outcome == Outcome::Flaky)
        .collect();
    if !flaky.is_empty() {
        text.push_str("\nFlaky cases (failed, then passed on a retry):\n\n");
        for result in flaky {
            text.push_str(&format!(
                "- {} {} {}: {}\n",
                result.case, result.model, result.backend, result.detail
            ));
        }
    }
    let mut updated: Vec<&str> = results
        .iter()
        .flat_map(|r| r.updated_references.iter().map(String::as_str))
        .collect();
    updated.sort();
    updated.dedup();
    if !updated.is_empty() {
        text.push_str(
            "\nReference images rewritten from the display (review them in the diff before committing):\n\n",
        );
        for path in updated {
            text.push_str(&format!("- `{path}`\n"));
        }
    }
    if let Some(cache) = cache.as_object().filter(|cache| !cache.is_empty()) {
        text.push_str("\nFixture cache (hits/builds):");
        for (kind, stats) in cache {
            text.push_str(&format!(" {kind} {}/{}", stats["hits"], stats["builds"]));
        }
        text.push('\n');
    }
    let mut file = std::fs::File::create(path).map_err(|e| e.to_string())?;
    file.write_all(text.as_bytes()).map_err(|e| e.to_string())
}

/// The results of a schema 1 or 2 report. A result that cannot be read is an
/// error: silently skipping it could drop a failure from `--rerun`.
pub fn read_results(path: &Path) -> Result<Vec<CaseResult>, String> {
    let report: Value = serde_json::from_slice(
        &std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?,
    )
    .map_err(|e| format!("{}: {e}", path.display()))?;
    let results = report["results"]
        .as_array()
        .ok_or_else(|| format!("{} has no results", path.display()))?;
    results
        .iter()
        .enumerate()
        .map(|(index, result)| {
            let mut result = result.clone();
            // Schema 1 results have no phase.
            if result.get("phase").is_none() {
                result["phase"] = "execute".into();
            }
            serde_json::from_value(result)
                .map_err(|e| format!("{} result {index}: {e}", path.display()))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    #[test]
    fn stage_is_recorded_when_an_operation_fails() {
        let recorder = super::StageRecorder::default();
        let operation = || -> Result<(), &'static str> {
            let _timer = recorder.start(super::Stage::Card);
            Err("failed")
        };
        assert!(operation().is_err());
        let results = recorder.results();
        assert_eq!(results.len(), 1);
        assert!(matches!(results[0].stage, super::Stage::Card));
        assert!(results[0].seconds >= 0.0);
    }

    #[test]
    fn adaptive_events_preserve_parent_timing_and_record_child_outcome() {
        use smoke_core::progress::{AttemptId, CaseEvent, CaseId, Outcome, VERSION, Writer};
        let path = std::env::temp_dir().join(format!("l6-adaptive-timing-{}", std::process::id()));
        let mut writer = Writer::create(&path).unwrap();
        let id = AttemptId {
            case: CaseId {
                case: "case".into(),
                model: l6fw::Model::L6max,
                backend: smoke_core::Backend::Headless,
            },
            attempt: 1,
        };
        writer
            .emit(&CaseEvent::Header {
                version: VERSION,
                run_id: "run".into(),
                id,
            })
            .unwrap();
        writer
            .emit(&CaseEvent::StepStarted {
                index: 2,
                label: "parent".into(),
                started_unix_ms: 100,
            })
            .unwrap();
        writer
            .emit(&CaseEvent::AdaptiveScopeStarted {
                index: 2,
                block: "pan".into(),
                attempt: Some(1),
                max_attempts: 100,
                started_unix_ms: 110,
            })
            .unwrap();
        writer
            .emit(&CaseEvent::AdaptiveChildStarted {
                index: 2,
                block: "pan".into(),
                attempt: 1,
                max_attempts: 100,
                child: "capture".into(),
                started_unix_ms: 120,
            })
            .unwrap();
        writer
            .emit(&CaseEvent::AdaptiveChildFinished {
                index: 2,
                block: "pan".into(),
                attempt: 1,
                child: "capture".into(),
                outcome: Outcome::Fail,
                finished_unix_ms: 150,
            })
            .unwrap();
        writer
            .emit(&CaseEvent::AdaptiveScopeFinished {
                index: 2,
                block: "pan".into(),
                attempt: Some(1),
                outcome: Outcome::Fail,
                matched: None,
                finished_unix_ms: 160,
            })
            .unwrap();
        for (attempt, matched, started, finished) in [(2, false, 165, 170), (3, true, 175, 180)] {
            writer
                .emit(&CaseEvent::AdaptiveScopeStarted {
                    index: 2,
                    block: "pan".into(),
                    attempt: Some(attempt),
                    max_attempts: 100,
                    started_unix_ms: started,
                })
                .unwrap();
            writer
                .emit(&CaseEvent::AdaptiveScopeFinished {
                    index: 2,
                    block: "pan".into(),
                    attempt: Some(attempt),
                    outcome: Outcome::Pass,
                    matched: Some(matched),
                    finished_unix_ms: finished,
                })
                .unwrap();
        }
        writer
            .emit(&CaseEvent::ScenarioFinished {
                failure: None,
                outcome: Outcome::Fail,
                finished_unix_ms: 200,
            })
            .unwrap();
        assert_eq!(super::step_timings(&path), vec![(2, "parent".into(), 100)]);
        assert_eq!(
            super::adaptive_child_timings(&path),
            vec![(2, "pan".into(), 1, "capture".into(), Outcome::Fail, 30)]
        );
        assert_eq!(
            super::adaptive_scope_timings(&path),
            vec![
                (2, "pan".into(), Some(1), Outcome::Fail, None, 50),
                (2, "pan".into(), Some(2), Outcome::Pass, Some(false), 5),
                (2, "pan".into(), Some(3), Outcome::Pass, Some(true), 5),
            ]
        );
        drop(writer);
        std::fs::remove_file(path).unwrap();
    }
    #[test]
    fn timing_report_orders_steps_and_omits_interrupted_final_step() {
        use smoke_core::progress::{AttemptId, CaseEvent, CaseId, VERSION, Writer};
        let path = std::env::temp_dir().join(format!("l6-step-timing-{}", std::process::id()));
        let mut writer = Writer::create(&path).unwrap();
        writer
            .emit(&CaseEvent::Header {
                version: VERSION,
                run_id: "run".into(),
                id: AttemptId {
                    case: CaseId {
                        case: "boot".into(),
                        model: l6fw::Model::L6,
                        backend: smoke_core::Backend::Headless,
                    },
                    attempt: 1,
                },
            })
            .unwrap();
        writer
            .emit(&CaseEvent::PhaseChanged {
                phase: smoke_core::progress::CasePhase::PreparingScenario,
                started_unix_ms: 100,
            })
            .unwrap();
        writer
            .emit(&CaseEvent::PhaseChanged {
                phase: smoke_core::progress::CasePhase::StartingEmulator,
                started_unix_ms: 400,
            })
            .unwrap();
        writer
            .emit(&CaseEvent::StepStarted {
                index: 1,
                label: "first".into(),
                started_unix_ms: 1000,
            })
            .unwrap();
        assert_eq!(
            super::case_phase_timings(&path),
            vec![
                (smoke_core::progress::CasePhase::PreparingScenario, 300),
                (smoke_core::progress::CasePhase::StartingEmulator, 600),
            ]
        );
        writer
            .emit(&CaseEvent::StepStarted {
                index: 2,
                label: "second".into(),
                started_unix_ms: 2000,
            })
            .unwrap();
        writer
            .emit(&CaseEvent::StepStarted {
                index: 3,
                label: "interrupted".into(),
                started_unix_ms: 5000,
            })
            .unwrap();
        assert_eq!(
            super::step_timings(&path),
            vec![(2, "second".into(), 3000), (1, "first".into(), 1000)]
        );
        writer
            .emit(&CaseEvent::ScenarioFinished {
                failure: None,
                outcome: smoke_core::progress::Outcome::Fail,
                finished_unix_ms: 5500,
            })
            .unwrap();
        assert_eq!(
            super::step_timings(&path)[2],
            (3, "interrupted".into(), 500)
        );
        assert_eq!(super::child_shutdown_seconds(&path, 7000), Some(1.5));
        drop(writer);
        std::fs::remove_file(path).unwrap();
    }
    use super::*;

    fn result(outcome: Outcome, phase: Phase) -> CaseResult {
        CaseResult {
            outcome,
            phase,
            detail: format!("{outcome}"),
            duration_s: 1.0,
            ..CaseResult::new(
                "case",
                Model::L6max,
                Backend::Headless,
                Path::new("/run/case"),
            )
        }
    }

    #[test]
    fn errors_before_the_case_runs_are_blocked_and_later_ones_fail() {
        let app: Result<(), String> = Err("no app".into());
        assert_eq!(
            app.blocked_in(Phase::Preflight).unwrap_err().outcome,
            Outcome::Blocked
        );
        let build: Result<(), String> = Err("l6patch failed".into());
        let error = build.failed_in(Phase::Fixture).unwrap_err();
        assert_eq!(
            (error.outcome, error.phase),
            (Outcome::Fail, Phase::Fixture)
        );
    }

    #[test]
    fn a_failure_then_a_pass_is_flaky_and_keeps_every_attempt() {
        let outcomes = [Outcome::Fail, Outcome::Pass];
        let flaky = with_retries(2, |n| {
            let mut result = result(outcomes[n - 1], Phase::Execute);
            result.timings.stages.push(super::StageTiming {
                stage: super::Stage::Card,
                seconds: n as f64,
            });
            result
        });
        assert_eq!(flaky.outcome, Outcome::Flaky);
        assert_eq!(flaky.attempts.len(), 2);
        assert_eq!(flaky.duration_s, 2.0);
        assert_eq!(flaky.attempts[0].timings.stages[0].seconds, 1.0);
        assert_eq!(flaky.attempts[1].timings.stages[0].seconds, 2.0);
        assert!(flaky.detail.contains("attempt 2"));
    }

    #[test]
    fn retries_stop_at_the_limit_and_skip_blocked_and_fixture_failures() {
        let mut runs = 0;
        let failed = with_retries(2, |_| {
            runs += 1;
            result(Outcome::Fail, Phase::Execute)
        });
        assert_eq!(
            (failed.outcome, runs, failed.attempts.len()),
            (Outcome::Fail, 3, 3)
        );
        for (outcome, phase) in [
            (Outcome::Blocked, Phase::Preflight),
            (Outcome::Fail, Phase::Fixture),
            (Outcome::Pass, Phase::Execute),
        ] {
            let mut runs = 0;
            let single = with_retries(2, |_| {
                runs += 1;
                result(outcome, phase)
            });
            assert_eq!((single.outcome, runs), (outcome, 1));
            assert!(single.attempts.is_empty());
        }
    }

    #[test]
    fn a_later_attempt_never_hides_an_earlier_failure() {
        use Outcome::*;
        for (second, expected) in [(Xfail, Fail), (Blocked, Fail), (Xpass, Fail), (Pass, Flaky)] {
            let outcomes = [Fail, second];
            let phases = [Phase::Execute, Phase::Execute];
            let result = with_retries(1, |n| result(outcomes[n - 1], phases[n - 1]));
            assert_eq!(result.outcome, expected, "FAIL then {second}");
            assert_eq!(result.attempts.len(), 2);
        }
    }

    #[test]
    fn the_run_outcome_ranks_failures_then_blocked_then_flaky() {
        let run = |outcomes: &[Outcome], strict| {
            let results: Vec<_> = outcomes
                .iter()
                .map(|o| result(*o, Phase::Execute))
                .collect();
            summarize(&results, strict)
        };
        use Outcome::*;
        assert_eq!(run(&[Pass, Blocked, Fail], false), (Fail, 1));
        assert_eq!(run(&[Pass, Xpass], false), (Fail, 1));
        assert_eq!(run(&[Pass, Blocked, Flaky], false), (Blocked, 2));
        assert_eq!(run(&[Pass, Flaky], false), (Flaky, 0));
        assert_eq!(run(&[Pass, Flaky], true), (Flaky, 1));
        assert_eq!(run(&[Pass, Xfail], false), (Xfail, 0));
        assert_eq!(run(&[Pass], false), (Pass, 0));
    }

    #[test]
    fn schema_2_results_keep_the_schema_1_fields() {
        let value = serde_json::to_value(result(Outcome::Pass, Phase::Execute)).unwrap();
        for field in ["case", "model", "backend", "outcome", "detail", "artifacts"] {
            assert!(value.get(field).is_some(), "{field}");
        }
        assert_eq!(value["outcome"], "PASS");
        assert_eq!(value["phase"], "execute");
    }
}
