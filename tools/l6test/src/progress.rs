//! Suite event writer and replaying live watcher.
use crate::report;
use smoke_core::progress::{self, AttemptId, CaseEvent, CaseId, Outcome, SuiteEvent, Writer};
use std::{
    collections::BTreeMap,
    ffi::OsString,
    fs::File,
    io::{self, IsTerminal, Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
    sync::Mutex,
    thread,
    time::Duration,
};

fn outcome(o: report::Outcome) -> Outcome {
    match o {
        report::Outcome::Pass => Outcome::Pass,
        report::Outcome::Fail => Outcome::Fail,
        report::Outcome::Flaky => Outcome::Flaky,
        report::Outcome::Blocked => Outcome::Blocked,
        report::Outcome::Xfail => Outcome::Xfail,
        report::Outcome::Xpass => Outcome::Xpass,
    }
}

pub struct Journal {
    writer: Mutex<Option<Writer>>,
}
impl Journal {
    pub fn new(dir: &Path, total: usize) -> Self {
        let run_id = dir.display().to_string();
        let writer = Writer::create(&dir.join("progress.jsonl"))
            .and_then(|mut writer| {
                writer.emit(&SuiteEvent::Header {
                    version: progress::VERSION,
                    run_id: run_id.clone(),
                })?;
                writer.emit(&SuiteEvent::RunStarted { total_cases: total })?;
                Ok(writer)
            })
            .map_err(|e| tracing::warn!(error = %e, "could not create progress stream"))
            .ok();
        Self {
            writer: Mutex::new(writer),
        }
    }
    fn emit(&self, event: SuiteEvent) {
        if let Ok(mut guard) = self.writer.lock()
            && let Some(writer) = guard.as_mut()
            && let Err(error) = writer.emit(&event)
        {
            tracing::warn!(%error, "could not write progress event");
            *guard = None;
        }
    }
    pub fn attempt_started(&self, id: AttemptId, path: PathBuf) {
        self.emit(SuiteEvent::AttemptStarted {
            id,
            progress_path: path,
        });
    }
    pub fn attempt_phase(&self, id: AttemptId, phase: progress::Phase) {
        self.emit(SuiteEvent::AttemptPhaseChanged { id, phase });
    }
    pub fn attempt_finished(&self, id: AttemptId, result: report::Outcome) {
        self.emit(SuiteEvent::AttemptFinished {
            id,
            outcome: outcome(result),
        });
    }
    pub fn case_finished(&self, id: CaseId, result: report::Outcome) {
        self.emit(SuiteEvent::CaseFinished {
            id,
            outcome: outcome(result),
        });
    }
    pub fn complete(&self, result: report::Outcome) {
        self.emit(SuiteEvent::RunFinished {
            outcome: outcome(result),
        });
    }
}

#[derive(Default)]
struct Tail {
    offset: u64,
    partial: Vec<u8>,
}
impl Tail {
    fn read<T: serde::de::DeserializeOwned>(&mut self, path: &Path) -> Result<Vec<T>, String> {
        let mut file = match File::open(path) {
            Ok(file) => file,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(vec![]),
            Err(e) => return Err(format!("{}: {e}", path.display())),
        };
        file.seek(SeekFrom::Start(self.offset))
            .map_err(|e| e.to_string())?;
        let mut bytes = Vec::new();
        file.read_to_end(&mut bytes).map_err(|e| e.to_string())?;
        self.offset += bytes.len() as u64;
        self.partial.extend(bytes);
        let mut result = Vec::new();
        while let Some(end) = self.partial.iter().position(|b| *b == b'\n') {
            let line: Vec<u8> = self.partial.drain(..=end).collect();
            result.push(
                serde_json::from_slice(&line)
                    .map_err(|e| format!("{}: invalid progress record: {e}", path.display()))?,
            );
        }
        Ok(result)
    }
}

struct Active {
    id: AttemptId,
    path: PathBuf,
    tail: Tail,
    header: bool,
    total: Option<usize>,
    step: Option<(usize, String)>,
    phase: Option<progress::Phase>,
    case_phase: Option<progress::CasePhase>,
    error: Option<String>,
}
#[derive(Default)]
struct View {
    run_id: Option<String>,
    total: Option<usize>,
    cases: BTreeMap<CaseId, Outcome>,
    active: BTreeMap<CaseId, Active>,
    outcome: Option<Outcome>,
    incomplete: bool,
}
impl View {
    fn suite(&mut self, event: SuiteEvent) -> Result<(), String> {
        if self.run_id.is_none() {
            return match event {
                SuiteEvent::Header { version, run_id } if version == progress::VERSION => {
                    self.run_id = Some(run_id);
                    Ok(())
                }
                SuiteEvent::Header { version, .. } => Err(format!(
                    "progress version mismatch: run uses {version}, watcher uses {}",
                    progress::VERSION
                )),
                _ => Err("progress stream is missing its header".into()),
            };
        }
        match event {
            SuiteEvent::Header { .. } => return Err("duplicate progress header".into()),
            SuiteEvent::RunStarted { total_cases } => self.total = Some(total_cases),
            SuiteEvent::AttemptStarted { id, progress_path } => {
                self.active.insert(
                    id.case.clone(),
                    Active {
                        id,
                        path: progress_path,
                        tail: Tail::default(),
                        header: false,
                        total: None,
                        step: None,
                        phase: None,
                        case_phase: None,
                        error: None,
                    },
                );
            }
            SuiteEvent::AttemptPhaseChanged { id, phase } => {
                if let Some(active) = self.active.get_mut(&id.case)
                    && active.id == id
                {
                    active.phase = Some(phase);
                }
            }
            SuiteEvent::AttemptFinished { id, .. } => {
                if self.active.get(&id.case).is_some_and(|a| a.id == id) {
                    self.active.remove(&id.case);
                }
            }
            SuiteEvent::CaseFinished { id, outcome } => {
                self.active.remove(&id);
                self.cases.insert(id, outcome);
            }
            SuiteEvent::RunFinished { outcome } => self.outcome = Some(outcome),
        }
        Ok(())
    }
    fn read_cases(&mut self, root: &Path) -> Result<(), String> {
        for active in self.active.values_mut() {
            if active.error.is_some() {
                continue;
            }
            let events = match active.tail.read::<CaseEvent>(&root.join(&active.path)) {
                Ok(events) => events,
                Err(error) => {
                    active.error = Some(error);
                    continue;
                }
            };
            for event in events {
                if !active.header {
                    match event {
                        CaseEvent::Header {
                            version,
                            run_id,
                            id,
                        } if version == progress::VERSION
                            && self.run_id.as_deref() == Some(&run_id)
                            && id == active.id =>
                        {
                            active.header = true
                        }
                        CaseEvent::Header { version, .. } if version != progress::VERSION => {
                            return Err(format!(
                                "progress version mismatch: case uses {version}, watcher uses {}",
                                progress::VERSION
                            ));
                        }
                        _ => {
                            active.error = Some("invalid progress stream identity".into());
                            break;
                        }
                    }
                    continue;
                }
                match event {
                    CaseEvent::PhaseChanged { phase } => active.case_phase = Some(phase),
                    CaseEvent::ScenarioStarted { total_steps } => active.total = Some(total_steps),
                    CaseEvent::StepStarted { index, label } => active.step = Some((index, label)),
                    CaseEvent::ScenarioFinished { .. } => {}
                    CaseEvent::Header { .. } => {
                        active.error = Some("duplicate case progress header".into());
                        break;
                    }
                }
            }
        }
        Ok(())
    }
}

fn render(view: &View, color: bool) -> String {
    let (cyan, green, red, yellow, reset) = if color {
        ("\x1b[1;36m", "\x1b[32m", "\x1b[31m", "\x1b[33m", "\x1b[0m")
    } else {
        ("", "", "", "", "")
    };
    let total = view.total.unwrap_or(0);
    let done = view.cases.len();
    let filled = if total == 0 {
        0
    } else {
        (done.saturating_mul(30) / total).min(30)
    };
    let percent = if total == 0 {
        0
    } else {
        done.saturating_mul(100) / total
    };
    let count = |outcome| view.cases.values().filter(|o| **o == outcome).count();
    let status = view
        .outcome
        .as_ref()
        .map(|o| format!("{o:?}").to_uppercase())
        .unwrap_or_else(|| "RUNNING".into());
    let mut s = format!(
        "  {cyan}L6 test suite{reset}  {status}\n  {}\n  {cyan}{}{}{reset}  {done}/{total} ({percent}%)\n  {green}✓ {} passed{reset}   {red}✗ {} failed{reset}   {yellow}! {} flaky   {} blocked{reset}\n",
        "━".repeat(42),
        "█".repeat(filled),
        "░".repeat(30 - filled),
        count(Outcome::Pass),
        count(Outcome::Fail),
        count(Outcome::Flaky),
        count(Outcome::Blocked)
    );
    if count(Outcome::Xfail) + count(Outcome::Xpass) > 0 {
        s.push_str(&format!(
            "  {} expected failures   {} unexpected passes\n",
            count(Outcome::Xfail),
            count(Outcome::Xpass)
        ));
    }
    if view.incomplete {
        s.push_str("  Progress stream ended early; see report.json.\n");
    }
    for a in view.active.values() {
        s.push_str(&format!(
            "\n  Running: {} ({}, {})",
            a.id.case.case, a.id.case.model, a.id.case.backend
        ));
        if let Some(error) = &a.error {
            s.push_str(&format!("\n    Progress unavailable: {error}"));
        }
        if a.id.attempt > 1 {
            s.push_str(&format!(" · attempt {}", a.id.attempt));
        }
        if a.step.is_none()
            && let Some(phase) = &a.case_phase
        {
            let label = match phase {
                progress::CasePhase::StartingEmulator => "Starting emulator",
                progress::CasePhase::PreparingScenario => "Preparing scenario",
            };
            s.push_str(&format!("\n    {label}"));
        } else if a.step.is_none()
            && let Some(phase) = &a.phase
        {
            let label = match phase {
                progress::Phase::PreparingFixture => "Preparing fixture",
                progress::Phase::Executing => "Starting scenario",
            };
            s.push_str(&format!("\n    {label}"));
        }
        if let (Some(total), Some((index, label))) = (a.total, &a.step) {
            s.push_str(&format!("\n    Step {index}/{total} · {label}"));
        }
    }
    s.push('\n');
    s
}

pub fn watch(args: &[OsString]) -> Result<(), String> {
    if args.len() != 1 {
        return Err("Usage: cargo l6test watch RUN_DIRECTORY".into());
    }
    let path = PathBuf::from(&args[0]).join("progress.jsonl");
    if !path.exists() {
        return Err(format!(
            "{}: progress stream not found; the run may use an older l6test",
            path.display()
        ));
    }
    let terminal = io::stdout().is_terminal();
    let mut tail = Tail::default();
    let mut view = View::default();
    let mut final_report_seen = false;
    loop {
        for event in tail.read::<SuiteEvent>(&path)? {
            view.suite(event)?;
        }
        view.read_cases(Path::new(&args[0]))?;
        if view.outcome.is_none() && view.run_id.is_some() {
            let report = Path::new(&args[0]).join("report.json");
            if let Ok(bytes) = std::fs::read(report)
                && let Ok(value) = serde_json::from_slice::<serde_json::Value>(&bytes)
                && let Some(status) = value.get("outcome")
                && let Ok(outcome) = serde_json::from_value::<Outcome>(status.clone())
            {
                if !terminal || final_report_seen {
                    view.outcome = Some(outcome);
                    view.incomplete = true;
                    view.active.clear();
                } else {
                    final_report_seen = true;
                }
            }
        }
        if terminal {
            print!("\x1b[2J\x1b[H");
        }
        print!(
            "{}",
            render(&view, terminal && std::env::var_os("NO_COLOR").is_none())
        );
        if terminal && view.outcome.is_none() {
            println!("\n  Ctrl+C to close · tests keep running");
        }
        io::stdout().flush().map_err(|e| e.to_string())?;
        if !terminal || view.outcome.is_some() {
            return Ok(());
        }
        thread::sleep(Duration::from_secs(2));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    fn id() -> CaseId {
        CaseId {
            case: "boot".into(),
            model: l6fw::Model::L6,
            backend: smoke_core::Backend::Headless,
        }
    }
    #[test]
    fn replay_tracks_retries_and_counts_the_final_case_once() {
        let mut view = View::default();
        view.suite(SuiteEvent::Header {
            version: progress::VERSION,
            run_id: "run".into(),
        })
        .unwrap();
        view.suite(SuiteEvent::RunStarted { total_cases: 1 })
            .unwrap();
        let first = AttemptId {
            case: id(),
            attempt: 1,
        };
        view.suite(SuiteEvent::AttemptStarted {
            id: first.clone(),
            progress_path: "one".into(),
        })
        .unwrap();
        view.suite(SuiteEvent::AttemptFinished {
            id: first,
            outcome: Outcome::Fail,
        })
        .unwrap();
        let second = AttemptId {
            case: id(),
            attempt: 2,
        };
        view.suite(SuiteEvent::AttemptStarted {
            id: second.clone(),
            progress_path: "two".into(),
        })
        .unwrap();
        assert!(render(&view, false).contains("attempt 2"));
        view.suite(SuiteEvent::AttemptFinished {
            id: second,
            outcome: Outcome::Pass,
        })
        .unwrap();
        view.suite(SuiteEvent::CaseFinished {
            id: id(),
            outcome: Outcome::Flaky,
        })
        .unwrap();
        assert_eq!(view.cases.len(), 1);
        assert!(render(&view, false).contains("1 flaky"));
        assert!(view.active.is_empty());
    }
    #[test]
    fn rejects_version_mismatch_and_missing_header() {
        let mut view = View::default();
        assert!(
            view.suite(SuiteEvent::Header {
                version: progress::VERSION + 1,
                run_id: "run".into()
            })
            .unwrap_err()
            .contains("version mismatch")
        );
        assert!(
            view.suite(SuiteEvent::RunStarted { total_cases: 1 })
                .is_err()
        );
    }
    #[test]
    fn late_attach_replays_step() {
        let dir = std::env::temp_dir().join(format!("l6-progress-case-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("attempt.jsonl");
        let attempt = AttemptId {
            case: id(),
            attempt: 1,
        };
        let mut writer = Writer::create(&path).unwrap();
        writer
            .emit(&CaseEvent::Header {
                version: progress::VERSION,
                run_id: "run".into(),
                id: attempt.clone(),
            })
            .unwrap();
        writer
            .emit(&CaseEvent::ScenarioStarted { total_steps: 5 })
            .unwrap();
        writer
            .emit(&CaseEvent::StepStarted {
                index: 3,
                label: "press control".into(),
            })
            .unwrap();
        let mut view = View::default();
        view.suite(SuiteEvent::Header {
            version: progress::VERSION,
            run_id: "run".into(),
        })
        .unwrap();
        view.suite(SuiteEvent::RunStarted { total_cases: 1 })
            .unwrap();
        view.suite(SuiteEvent::AttemptStarted {
            id: attempt,
            progress_path: "attempt.jsonl".into(),
        })
        .unwrap();
        view.read_cases(&dir).unwrap();
        assert!(render(&view, false).contains("Step 3/5 · press control"));
        drop(writer);
        std::fs::remove_dir_all(dir).unwrap();
    }
    #[test]
    fn live_case_phase_is_replaced_by_its_step() {
        let dir = std::env::temp_dir().join(format!("l6-progress-phase-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let mut writer = Writer::create(&dir.join("attempt.jsonl")).unwrap();
        let attempt = AttemptId {
            case: id(),
            attempt: 1,
        };
        writer
            .emit(&CaseEvent::Header {
                version: progress::VERSION,
                run_id: "run".into(),
                id: attempt.clone(),
            })
            .unwrap();
        writer
            .emit(&CaseEvent::PhaseChanged {
                phase: progress::CasePhase::StartingEmulator,
            })
            .unwrap();
        let mut view = View::default();
        view.suite(SuiteEvent::Header {
            version: progress::VERSION,
            run_id: "run".into(),
        })
        .unwrap();
        view.suite(SuiteEvent::AttemptStarted {
            id: attempt,
            progress_path: "attempt.jsonl".into(),
        })
        .unwrap();
        view.read_cases(&dir).unwrap();
        assert!(render(&view, false).contains("Starting emulator"));
        writer
            .emit(&CaseEvent::ScenarioStarted { total_steps: 2 })
            .unwrap();
        writer
            .emit(&CaseEvent::StepStarted {
                index: 1,
                label: "Wait for boot ready".into(),
            })
            .unwrap();
        view.read_cases(&dir).unwrap();
        let text = render(&view, false);
        assert!(text.contains("Step 1/2 · Wait for boot ready"));
        assert!(!text.contains("Starting emulator"));
        drop(writer);
        std::fs::remove_dir_all(dir).unwrap();
    }
    #[test]
    fn case_version_mismatch_refuses_watching() {
        let dir = std::env::temp_dir().join(format!("l6-progress-version-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("attempt.jsonl");
        let attempt = AttemptId {
            case: id(),
            attempt: 1,
        };
        let mut writer = Writer::create(&path).unwrap();
        writer
            .emit(&CaseEvent::Header {
                version: progress::VERSION + 1,
                run_id: "run".into(),
                id: attempt.clone(),
            })
            .unwrap();
        let mut view = View::default();
        view.suite(SuiteEvent::Header {
            version: progress::VERSION,
            run_id: "run".into(),
        })
        .unwrap();
        view.suite(SuiteEvent::AttemptStarted {
            id: attempt,
            progress_path: "attempt.jsonl".into(),
        })
        .unwrap();
        assert!(
            view.read_cases(&dir)
                .unwrap_err()
                .contains("version mismatch")
        );
        drop(writer);
        std::fs::remove_dir_all(dir).unwrap();
    }
    #[test]
    fn tail_waits_for_complete_lines_and_never_replays_them() {
        let path = std::env::temp_dir().join(format!("l6-progress-tail-{}", std::process::id()));
        let mut file = std::fs::File::create(&path).unwrap();
        write!(file, "{{\"event\":\"run_started\",").unwrap();
        file.flush().unwrap();
        let mut tail = Tail::default();
        assert!(tail.read::<SuiteEvent>(&path).unwrap().is_empty());
        writeln!(file, "\"total_cases\":2}}").unwrap();
        file.flush().unwrap();
        assert_eq!(
            tail.read::<SuiteEvent>(&path).unwrap(),
            vec![SuiteEvent::RunStarted { total_cases: 2 }]
        );
        assert!(tail.read::<SuiteEvent>(&path).unwrap().is_empty());
        std::fs::remove_file(path).unwrap();
    }
}
