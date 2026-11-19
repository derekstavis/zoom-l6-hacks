//! Case results, run outcome and the files that report them.
//!
//! `report.json` schema 2 keeps every schema 1 field of a result (`case`,
//! `model`, `backend`, `outcome`, `detail`, `artifacts`) and adds the phase a
//! case stopped in, durations, attempts, input identities and reuse.
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
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
    /// Not run: an earlier run passed with identical inputs.
    Reused,
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
            Outcome::Reused => "REUSED",
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
    Reuse,
}

/// An error from one phase of a case.
#[derive(Debug)]
pub struct CaseError {
    pub phase: Phase,
    pub message: String,
}

impl CaseError {
    pub fn preflight(message: impl Into<String>) -> Self {
        Self {
            phase: Phase::Preflight,
            message: message.into(),
        }
    }
    /// The outcome this error gives its case.
    pub fn outcome(&self) -> Outcome {
        match self.phase {
            Phase::Preflight => Outcome::Blocked,
            Phase::Fixture | Phase::Execute | Phase::Reuse => Outcome::Fail,
        }
    }
}

/// Attach a phase to a fallible step: `step().phase(Phase::Fixture)?`.
pub trait InPhase<T> {
    fn phase(self, phase: Phase) -> Result<T, CaseError>;
}

impl<T> InPhase<T> for Result<T, String> {
    fn phase(self, phase: Phase) -> Result<T, CaseError> {
        self.map_err(|message| CaseError { phase, message })
    }
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct Timings {
    pub fixture_s: f64,
    pub execute_s: f64,
}

/// The earlier passing result a reused case stands on.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Reused {
    /// The run directory that executed the case.
    pub run: PathBuf,
    pub artifacts: PathBuf,
    pub outcome: Outcome,
}

/// One execution of a case. A case without retries has one attempt.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Attempt {
    pub outcome: Outcome,
    pub phase: Phase,
    pub detail: String,
    pub artifacts: PathBuf,
    pub duration_s: f64,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct CaseResult {
    pub case: String,
    pub model: String,
    pub backend: String,
    pub outcome: Outcome,
    pub detail: String,
    pub artifacts: PathBuf,
    pub phase: Phase,
    #[serde(default)]
    pub duration_s: f64,
    #[serde(default)]
    pub timings: Timings,
    /// Hashes of everything that decides this case's result.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub identity: Option<Value>,
    /// Digest of `identity`; equal keys mean identical inputs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reuse_key: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reused: Option<Reused>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub attempts: Vec<Attempt>,
}

impl CaseResult {
    pub fn new(case: &str, model: &str, backend: &str, artifacts: &Path) -> Self {
        Self {
            case: case.into(),
            model: model.into(),
            backend: backend.into(),
            outcome: Outcome::Blocked,
            detail: String::new(),
            artifacts: artifacts.into(),
            phase: Phase::Preflight,
            duration_s: 0.0,
            timings: Timings::default(),
            identity: None,
            reuse_key: None,
            reused: None,
            attempts: Vec::new(),
        }
    }
    /// The outcome that counts toward the run: a reused case counts as the
    /// outcome it was reused from.
    pub fn effective(&self) -> Outcome {
        match (&self.outcome, &self.reused) {
            (Outcome::Reused, Some(reused)) => reused.outcome,
            (outcome, _) => *outcome,
        }
    }
    fn attempt(&self) -> Attempt {
        Attempt {
            outcome: self.outcome,
            phase: self.phase,
            detail: self.detail.clone(),
            artifacts: self.artifacts.clone(),
            duration_s: self.duration_s,
        }
    }
}

/// Run `attempt` until it stops failing in execution, at most `1 + retries`
/// times. A failure followed by a pass is FLAKY. Only execution failures are
/// retried: a BLOCKED case did not run, and fixture builds are deterministic.
pub fn with_retries(retries: usize, mut attempt: impl FnMut(usize) -> CaseResult) -> CaseResult {
    let mut attempts = Vec::new();
    let mut number = 1;
    loop {
        let result = attempt(number);
        attempts.push(result.attempt());
        let retry = result.outcome == Outcome::Fail && result.phase == Phase::Execute;
        if !retry || number > retries {
            let mut result = result;
            let first_failure = attempts.iter().find(|a| a.outcome == Outcome::Fail);
            if result.outcome == Outcome::Pass
                && let Some(failure) = first_failure
            {
                result.outcome = Outcome::Flaky;
                result.detail = format!(
                    "passed on attempt {number} after failing: {}",
                    failure.detail
                );
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
    let has = |outcome: Outcome| results.iter().any(|r| r.effective() == outcome);
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

pub fn write_summary(
    path: &Path,
    outcome: Outcome,
    results: &[CaseResult],
    cache: &Value,
) -> Result<(), String> {
    let mut text = String::new();
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
    text.push_str("| Case | Model | Backend | Result | Phase | Seconds |\n");
    text.push_str("| --- | --- | --- | --- | --- | --- |\n");
    for result in results {
        let shown = match &result.reused {
            Some(reused) => format!("REUSED ({})", reused.outcome),
            None => result.outcome.to_string(),
        };
        text.push_str(&format!(
            "| {} | {} | {} | {} | {:?} | {:.1} |\n",
            result.case, result.model, result.backend, shown, result.phase, result.duration_s
        ));
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
    let reused: Vec<_> = results.iter().filter_map(|r| r.reused.as_ref()).collect();
    if !reused.is_empty() {
        let runs: std::collections::BTreeSet<_> = reused.iter().map(|r| &r.run).collect();
        text.push_str(&format!(
            "\n{} cases were not run: identical inputs passed in {}.\n",
            reused.len(),
            runs.iter()
                .map(|run| run.display().to_string())
                .collect::<Vec<_>>()
                .join(", ")
        ));
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

#[cfg(test)]
mod tests {
    use super::*;

    fn result(outcome: Outcome, phase: Phase) -> CaseResult {
        CaseResult {
            outcome,
            phase,
            detail: format!("{outcome}"),
            duration_s: 1.0,
            ..CaseResult::new("case", "l6max", "headless", Path::new("/run/case"))
        }
    }

    #[test]
    fn errors_before_the_case_runs_are_blocked_and_later_ones_fail() {
        assert_eq!(CaseError::preflight("no app").outcome(), Outcome::Blocked);
        let build: Result<(), String> = Err("l6patch failed".into());
        assert_eq!(
            build.phase(Phase::Fixture).unwrap_err().outcome(),
            Outcome::Fail
        );
    }

    #[test]
    fn a_failure_then_a_pass_is_flaky_and_keeps_every_attempt() {
        let outcomes = [Outcome::Fail, Outcome::Pass];
        let flaky = with_retries(2, |n| result(outcomes[n - 1], Phase::Execute));
        assert_eq!(flaky.outcome, Outcome::Flaky);
        assert_eq!(flaky.attempts.len(), 2);
        assert_eq!(flaky.duration_s, 2.0);
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
    fn a_reused_case_counts_as_the_outcome_it_was_reused_from() {
        let mut reused = result(Outcome::Reused, Phase::Reuse);
        reused.reused = Some(Reused {
            run: "/earlier".into(),
            artifacts: "/earlier/case".into(),
            outcome: Outcome::Xfail,
        });
        assert_eq!(reused.effective(), Outcome::Xfail);
        assert_eq!(summarize(&[reused], false), (Outcome::Xfail, 0));
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
