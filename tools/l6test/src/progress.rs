//! Suite event writer and replaying live watcher.
use crate::report;
use smoke_core::progress::{self, AttemptId, CaseEvent, CaseId, Outcome, SuiteEvent, Writer};
use std::{
    collections::{BTreeMap, BTreeSet},
    ffi::OsString,
    fs::File,
    io::{self, IsTerminal, Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
    sync::{Mutex, mpsc},
    thread,
    time::{Duration, Instant},
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
    pub fn run_phase(&self, phase: progress::RunPhase) {
        self.emit(SuiteEvent::RunPhaseChanged {
            phase,
            started_unix_ms: progress::unix_ms(),
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

#[derive(Clone, Default)]
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

#[derive(Clone)]
struct Active {
    id: AttemptId,
    path: PathBuf,
    tail: Tail,
    header: bool,
    total: Option<usize>,
    step: Option<(usize, String, u64)>,
    subprogress: Option<(u32, u32)>,
    adaptive: Option<(usize, String, u32, u32, String, u64)>,
    phase: Option<progress::Phase>,
    case_phase: Option<(progress::CasePhase, u64)>,
    error: Option<String>,
}
#[derive(Clone, Default)]
struct View {
    run_id: Option<String>,
    total: Option<usize>,
    cases: BTreeMap<CaseId, Outcome>,
    active: BTreeMap<CaseId, Active>,
    outcome: Option<Outcome>,
    incomplete: bool,
    run_phase: Option<(progress::RunPhase, u64)>,
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
            SuiteEvent::RunPhaseChanged {
                phase,
                started_unix_ms,
            } => self.run_phase = Some((phase, started_unix_ms)),
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
                        subprogress: None,
                        adaptive: None,
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
                    CaseEvent::PhaseChanged {
                        phase,
                        started_unix_ms,
                    } => active.case_phase = Some((phase, started_unix_ms)),
                    CaseEvent::ScenarioStarted { total_steps } => active.total = Some(total_steps),
                    CaseEvent::StepStarted {
                        index,
                        label,
                        started_unix_ms,
                    } => {
                        active.step = Some((index, label, started_unix_ms));
                        active.subprogress = None;
                        active.adaptive = None;
                    }
                    CaseEvent::StepProgress {
                        index,
                        completed,
                        total,
                    } => {
                        if total == 0
                            || completed > total
                            || !active.step.as_ref().is_some_and(|step| step.0 == index)
                        {
                            active.error = Some("invalid step progress".into());
                            break;
                        }
                        active.subprogress = Some((completed, total));
                    }
                    CaseEvent::AdaptiveScopeStarted { .. }
                    | CaseEvent::AdaptiveScopeFinished { .. } => {}
                    CaseEvent::AdaptiveChildStarted {
                        index,
                        block,
                        attempt,
                        max_attempts,
                        child,
                        started_unix_ms,
                    } => {
                        active.adaptive =
                            Some((index, block, attempt, max_attempts, child, started_unix_ms));
                    }
                    CaseEvent::AdaptiveChildFinished {
                        index,
                        block,
                        attempt,
                        child,
                        ..
                    } => {
                        if active.adaptive.as_ref().is_some_and(|(i, b, a, _, c, _)| {
                            *i == index && *b == block && *a == attempt && *c == child
                        }) {
                            active.adaptive = None;
                        }
                    }
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
    if view.run_id.is_none() {
        return "  Loading progress…\n".into();
    }
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
    if view.active.is_empty()
        && view.outcome.is_none()
        && let Some((phase, started)) = &view.run_phase
    {
        let label = match phase {
            progress::RunPhase::Building => "Building executables",
            progress::RunPhase::Staging => "Staging executables",
            progress::RunPhase::Executing => "Starting cases",
        };
        let seconds = progress::unix_ms().saturating_sub(*started) / 1000;
        s.push_str(&format!("\n  {label} · {seconds}s\n"));
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
            let label = match &phase.0 {
                progress::CasePhase::StartingEmulator => "Starting emulator",
                progress::CasePhase::PreparingScenario => "Preparing scenario",
            };
            let seconds = progress::unix_ms().saturating_sub(phase.1) / 1000;
            s.push_str(&format!("\n    {label} · {seconds}s"));
        } else if a.step.is_none()
            && let Some(phase) = &a.phase
        {
            let label = match phase {
                progress::Phase::PreparingFixture => "Preparing fixture",
                progress::Phase::Executing => "Starting scenario",
            };
            s.push_str(&format!("\n    {label}"));
        }
        if let (Some(total), Some((index, label, started))) = (a.total, &a.step) {
            let seconds = progress::unix_ms().saturating_sub(*started) / 1000;
            let count = a
                .subprogress
                .map(|(done, total)| format!(" · {done}/{total}"))
                .unwrap_or_default();
            s.push_str(&format!(
                "\n    Step {index}/{total} · {label}{count} · {seconds}s"
            ));
        }
        if let Some((_, block, attempt, max_attempts, child, started)) = &a.adaptive {
            let seconds = progress::unix_ms().saturating_sub(*started) / 1000;
            s.push_str(&format!(
                "\n      {block} · attempt {attempt}/{max_attempts} · {child} · {seconds}s"
            ));
        }
    }
    s.push('\n');
    s
}

/// Read and deserialize progress away from the terminal thread. Filesystem
/// notifications wake the reader; a bounded periodic check catches missed events
/// and filesystems without reliable notifications.
fn follow(root: PathBuf, terminal: bool, snapshots: mpsc::SyncSender<Result<View, String>>) {
    use notify::{RecursiveMode, Watcher};
    let (wake_tx, wake_rx) = mpsc::sync_channel::<()>(1);
    let mut watcher = notify::recommended_watcher(move |event: notify::Result<notify::Event>| {
        let relevant = match event {
            Ok(event) => {
                event.paths.is_empty()
                    || event.paths.iter().any(|path| {
                        path.is_dir()
                            || matches!(
                                path.file_name().and_then(|name| name.to_str()),
                                Some("progress.jsonl" | "report.json")
                            )
                    })
            }
            Err(_) => true,
        };
        if relevant {
            let _ = wake_tx.try_send(());
        }
    })
    .ok();
    if let Some(watcher) = watcher.as_mut()
        && watcher.watch(&root, RecursiveMode::NonRecursive).is_err()
    {
        watcher.unwatch(&root).ok();
    }
    let mut watched = BTreeSet::new();
    let mut tail = Tail::default();
    let mut view = View::default();
    let mut final_report_seen = false;
    loop {
        let read = (|| -> Result<(), String> {
            for event in tail.read::<SuiteEvent>(&root.join("progress.jsonl"))? {
                view.suite(event)?;
            }
            // Start watching each case directory before reading its stream, so
            // creation or appends cannot fall between replay and registration.
            if let Some(watcher) = watcher.as_mut() {
                let active_dirs: BTreeSet<PathBuf> = view
                    .active
                    .values()
                    .filter_map(|active| root.join(&active.path).parent().map(Path::to_path_buf))
                    .collect();
                for path in watched.difference(&active_dirs) {
                    let _ = watcher.unwatch(path);
                }
                watched.retain(|path| active_dirs.contains(path));
                for path in active_dirs {
                    if !watched.contains(&path)
                        && watcher.watch(&path, RecursiveMode::NonRecursive).is_ok()
                    {
                        watched.insert(path);
                    }
                }
            }
            view.read_cases(&root)?;
            if view.outcome.is_none()
                && view.run_id.is_some()
                && let Ok(bytes) = std::fs::read(root.join("report.json"))
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
            Ok(())
        })();
        match read {
            Ok(()) => {
                let done = view.outcome.is_some() || !terminal;
                if snapshots.send(Ok(view.clone())).is_err() || done {
                    return;
                }
            }
            Err(error) => {
                let _ = snapshots.send(Err(error));
                return;
            }
        }
        // Also catches missed notifications and lets an interrupted run's
        // report settle before deciding that its event stream ended early.
        match wake_rx.recv_timeout(Duration::from_secs(1)) {
            Ok(()) | Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => thread::sleep(Duration::from_secs(1)),
        }
    }
}

pub fn watch(args: &[OsString]) -> Result<(), String> {
    if args.len() != 1 {
        return Err("Usage: cargo l6test watch RUN_DIRECTORY".into());
    }
    let root = PathBuf::from(&args[0]);
    let path = root.join("progress.jsonl");
    if !path.exists() {
        return Err(format!(
            "{}: progress stream not found; the run may use an older l6test",
            path.display()
        ));
    }
    let terminal = io::stdout().is_terminal();
    let (tx, rx) = mpsc::sync_channel(1);
    thread::spawn(move || follow(root, terminal, tx));
    let mut view = if terminal {
        View::default()
    } else {
        rx.recv()
            .map_err(|_| "progress reader stopped".to_string())??
    };
    let mut last_draw = Instant::now();
    let mut redraw = true;
    loop {
        if redraw {
            if terminal {
                print!("\x1b[H\x1b[J");
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
            last_draw = Instant::now();
            redraw = false;
        }
        // Process a 100 ms UI tick independently of the reader. The current
        // screen only needs one-second clock updates; animated widgets can
        // request a redraw on every tick without affecting event processing.
        match rx.recv_timeout(Duration::from_millis(100)) {
            Ok(snapshot) => {
                view = snapshot?;
                redraw = true;
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {
                if last_draw.elapsed() >= Duration::from_secs(1) {
                    redraw = true;
                }
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                return Err("progress reader stopped".into());
            }
        }
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
    fn follower_replays_and_detects_new_case_streams() {
        let dir = std::env::temp_dir().join(format!("l6-progress-follower-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let mut suite = Writer::create(&dir.join("progress.jsonl")).unwrap();
        suite
            .emit(&SuiteEvent::Header {
                version: progress::VERSION,
                run_id: "run".into(),
            })
            .unwrap();
        suite
            .emit(&SuiteEvent::RunStarted { total_cases: 1 })
            .unwrap();
        let (tx, rx) = mpsc::sync_channel(1);
        let root = dir.clone();
        let worker = std::thread::spawn(move || follow(root, true, tx));
        assert_eq!(
            rx.recv_timeout(Duration::from_secs(3))
                .unwrap()
                .unwrap()
                .total,
            Some(1)
        );
        let attempt = AttemptId {
            case: id(),
            attempt: 1,
        };
        std::fs::create_dir(dir.join("case")).unwrap();
        let mut case = Writer::create(&dir.join("case/progress.jsonl")).unwrap();
        case.emit(&CaseEvent::Header {
            version: progress::VERSION,
            run_id: "run".into(),
            id: attempt.clone(),
        })
        .unwrap();
        case.emit(&CaseEvent::ScenarioStarted { total_steps: 1 })
            .unwrap();
        case.emit(&CaseEvent::StepStarted {
            index: 1,
            label: "test".into(),
            started_unix_ms: progress::unix_ms(),
        })
        .unwrap();
        suite
            .emit(&SuiteEvent::AttemptStarted {
                id: attempt,
                progress_path: "case/progress.jsonl".into(),
            })
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(4);
        loop {
            let view = rx
                .recv_timeout(deadline.saturating_duration_since(Instant::now()))
                .unwrap()
                .unwrap();
            if view
                .active
                .get(&id())
                .is_some_and(|active| active.step.is_some())
            {
                break;
            }
        }
        suite
            .emit(&SuiteEvent::RunFinished {
                outcome: Outcome::Pass,
            })
            .unwrap();
        loop {
            if rx
                .recv_timeout(Duration::from_secs(3))
                .unwrap()
                .unwrap()
                .outcome
                .is_some()
            {
                break;
            }
        }
        worker.join().unwrap();
        drop(case);
        drop(suite);
        std::fs::remove_dir_all(dir).unwrap();
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
    fn replay_shows_the_current_suite_setup_phase() {
        let mut view = View::default();
        view.suite(SuiteEvent::Header {
            version: progress::VERSION,
            run_id: "run".into(),
        })
        .unwrap();
        view.suite(SuiteEvent::RunStarted { total_cases: 1 })
            .unwrap();
        view.suite(SuiteEvent::RunPhaseChanged {
            phase: progress::RunPhase::Building,
            started_unix_ms: progress::unix_ms(),
        })
        .unwrap();
        assert!(render(&view, false).contains("Building executables"));
        view.suite(SuiteEvent::RunPhaseChanged {
            phase: progress::RunPhase::Staging,
            started_unix_ms: progress::unix_ms(),
        })
        .unwrap();
        let rendered = render(&view, false);
        assert!(rendered.contains("Staging executables"));
        assert!(!rendered.contains("Building executables"));
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
                started_unix_ms: progress::unix_ms().saturating_sub(12_000),
            })
            .unwrap();
        writer
            .emit(&CaseEvent::StepProgress {
                index: 3,
                completed: 17,
                total: 47,
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
        assert!(render(&view, false).contains("Step 3/5 · press control · 17/47 · 12s"));
        drop(writer);
        std::fs::remove_dir_all(dir).unwrap();
    }
    #[test]
    fn late_attach_replays_adaptive_child_without_replacing_parent_step() {
        let dir = std::env::temp_dir().join(format!("l6-progress-adaptive-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let attempt = AttemptId {
            case: id(),
            attempt: 1,
        };
        let mut writer = Writer::create(&dir.join("attempt.jsonl")).unwrap();
        writer
            .emit(&CaseEvent::Header {
                version: progress::VERSION,
                run_id: "run".into(),
                id: attempt.clone(),
            })
            .unwrap();
        writer
            .emit(&CaseEvent::ScenarioStarted { total_steps: 7 })
            .unwrap();
        writer
            .emit(&CaseEvent::StepStarted {
                index: 4,
                label: "adaptive".into(),
                started_unix_ms: progress::unix_ms(),
            })
            .unwrap();
        writer
            .emit(&CaseEvent::AdaptiveChildStarted {
                index: 4,
                block: "pan left".into(),
                attempt: 3,
                max_attempts: 100,
                child: "read pan".into(),
                started_unix_ms: progress::unix_ms(),
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
        let output = render(&view, false);
        assert!(output.contains("Step 4/7 · adaptive"));
        assert!(output.contains("pan left · attempt 3/100 · read pan"));
        writer
            .emit(&CaseEvent::AdaptiveChildFinished {
                index: 4,
                block: "pan left".into(),
                attempt: 3,
                child: "read pan".into(),
                outcome: Outcome::Pass,
                finished_unix_ms: progress::unix_ms(),
            })
            .unwrap();
        view.read_cases(&dir).unwrap();
        assert!(!render(&view, false).contains("attempt 3/100"));
        drop(writer);
        std::fs::remove_dir_all(dir).unwrap();
    }
    #[test]
    fn late_attach_replays_paced_and_adaptive_steps_in_one_stream() {
        let dir = std::env::temp_dir().join(format!("l6-progress-combined-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let attempt = AttemptId {
            case: id(),
            attempt: 1,
        };
        let mut writer = Writer::create(&dir.join("attempt.jsonl")).unwrap();
        writer
            .emit(&CaseEvent::Header {
                version: progress::VERSION,
                run_id: "run".into(),
                id: attempt.clone(),
            })
            .unwrap();
        writer
            .emit(&CaseEvent::ScenarioStarted { total_steps: 2 })
            .unwrap();
        writer
            .emit(&CaseEvent::StepStarted {
                index: 1,
                label: "effect".into(),
                started_unix_ms: progress::unix_ms(),
            })
            .unwrap();
        writer
            .emit(&CaseEvent::StepProgress {
                index: 1,
                completed: 17,
                total: 47,
            })
            .unwrap();
        writer
            .emit(&CaseEvent::StepStarted {
                index: 2,
                label: "adaptive".into(),
                started_unix_ms: progress::unix_ms(),
            })
            .unwrap();
        writer
            .emit(&CaseEvent::AdaptiveChildStarted {
                index: 2,
                block: "channel 2".into(),
                attempt: 2,
                max_attempts: 4,
                child: "wait for text".into(),
                started_unix_ms: progress::unix_ms(),
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
        let output = render(&view, false);
        assert!(output.contains("Step 2/2 · adaptive"));
        assert!(output.contains("channel 2 · attempt 2/4 · wait for text"));
        assert!(!output.contains("17/47"));
        writer
            .emit(&CaseEvent::AdaptiveChildFinished {
                index: 2,
                block: "channel 2".into(),
                attempt: 2,
                child: "wait for text".into(),
                outcome: Outcome::Pass,
                finished_unix_ms: progress::unix_ms(),
            })
            .unwrap();
        view.read_cases(&dir).unwrap();
        assert!(!render(&view, false).contains("attempt 2/4"));
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
                started_unix_ms: progress::unix_ms(),
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
                started_unix_ms: progress::unix_ms(),
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
