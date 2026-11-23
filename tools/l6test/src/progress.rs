//! Live progress shared between the suite driver and terminal watcher.
use crate::report::Outcome;
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    ffi::OsString,
    fs,
    io::{self, IsTerminal, Write},
    path::{Path, PathBuf},
    sync::Mutex,
    thread,
    time::Duration,
};

#[derive(Clone, Debug, Serialize, Deserialize)]
struct Active {
    case: String,
    model: String,
    backend: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct Snapshot {
    total: usize,
    completed: usize,
    outcome: String,
    counts: BTreeMap<String, usize>,
    active: BTreeMap<String, Active>,
}

pub struct Journal {
    path: PathBuf,
    state: Mutex<Snapshot>,
}
impl Journal {
    pub fn new(dir: &Path, total: usize) -> Result<Self, String> {
        let journal = Self {
            path: dir.join("progress.json"),
            state: Mutex::new(Snapshot {
                total,
                completed: 0,
                outcome: "RUNNING".into(),
                counts: BTreeMap::new(),
                active: BTreeMap::new(),
            }),
        };
        journal.save(&journal.state.lock().unwrap())?;
        Ok(journal)
    }
    fn save(&self, state: &Snapshot) -> Result<(), String> {
        let tmp = self.path.with_extension("json.tmp");
        fs::write(&tmp, serde_json::to_vec(state).map_err(|e| e.to_string())?)
            .map_err(|e| e.to_string())?;
        fs::rename(tmp, &self.path).map_err(|e| e.to_string())
    }
    pub fn start(&self, key: &str, case: &str, model: &str, backend: &str) {
        let mut state = self.state.lock().unwrap();
        state.active.insert(
            key.into(),
            Active {
                case: case.into(),
                model: model.into(),
                backend: backend.into(),
            },
        );
        if let Err(e) = self.save(&state) {
            tracing::warn!(error = %e, "could not write progress");
        }
    }
    pub fn finish(&self, key: &str, outcome: Outcome) {
        let mut state = self.state.lock().unwrap();
        state.active.remove(key);
        state.completed += 1;
        *state.counts.entry(outcome.to_string()).or_default() += 1;
        if let Err(e) = self.save(&state) {
            tracing::warn!(error = %e, "could not write progress");
        }
    }
    pub fn complete(&self, outcome: &str) -> Result<(), String> {
        let mut state = self.state.lock().unwrap();
        state.outcome = outcome.into();
        self.save(&state)
    }
}
impl Drop for Journal {
    fn drop(&mut self) {
        if let Ok(state) = self.state.get_mut()
            && state.outcome == "RUNNING"
        {
            state.outcome = "BLOCKED".into();
            state.active.clear();
            let snapshot = state.clone();
            let _ = self.save(&snapshot);
        }
    }
}

fn render(s: &Snapshot, color: bool) -> String {
    let (cyan, green, red, yellow, reset) = if color {
        ("\x1b[1;36m", "\x1b[32m", "\x1b[31m", "\x1b[33m", "\x1b[0m")
    } else {
        ("", "", "", "", "")
    };
    let filled = if s.total == 0 {
        0
    } else {
        s.completed
            .saturating_mul(30)
            .checked_div(s.total)
            .unwrap_or(0)
            .min(30)
    };
    let percent = if s.total == 0 {
        0
    } else {
        s.completed.saturating_mul(100) / s.total
    };
    let count = |key: &str| s.counts.get(key).copied().unwrap_or(0);
    let mut out = format!(
        "  {cyan}L6 test suite{reset}  {}\n  {}\n  {cyan}{}{}{reset}  {}/{} ({}%)\n  {green}✓ {} passed{reset}   {red}✗ {} failed{reset}   {yellow}! {} flaky   {} blocked{reset}\n",
        s.outcome,
        "━".repeat(42),
        "█".repeat(filled),
        "░".repeat(30 - filled),
        s.completed,
        s.total,
        percent,
        count("PASS"),
        count("FAIL"),
        count("FLAKY"),
        count("BLOCKED")
    );
    if count("XFAIL") + count("XPASS") > 0 {
        out.push_str(&format!(
            "  {} expected failures   {} unexpected passes\n",
            count("XFAIL"),
            count("XPASS")
        ));
    }
    for active in s.active.values() {
        out.push_str(&format!(
            "\n  Running: {} ({}, {})",
            active.case, active.model, active.backend
        ));
    }
    out.push('\n');
    out
}

pub fn watch(args: &[OsString]) -> Result<(), String> {
    if args.len() != 1 {
        return Err("Usage: cargo l6test watch RUN_DIRECTORY".into());
    }
    let path = PathBuf::from(&args[0]).join("progress.json");
    let terminal = io::stdout().is_terminal();
    loop {
        let bytes = fs::read(&path).map_err(|e| {
            format!(
                "{}: {e}; progress is available for runs started with this version of l6test",
                path.display()
            )
        })?;
        let state: Snapshot =
            serde_json::from_slice(&bytes).map_err(|e| format!("{}: {e}", path.display()))?;
        if terminal {
            print!("\x1b[2J\x1b[H");
        }
        print!(
            "{}",
            render(&state, terminal && std::env::var_os("NO_COLOR").is_none())
        );
        if terminal && state.outcome == "RUNNING" {
            println!("\n  Ctrl+C to close · tests keep running");
        }
        io::stdout().flush().map_err(|e| e.to_string())?;
        if !terminal || state.outcome != "RUNNING" {
            return Ok(());
        }
        thread::sleep(Duration::from_secs(2));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn journal_tracks_overlapping_cases_and_preserves_flaky_results() {
        let dir = std::env::temp_dir().join(format!("l6test-progress-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let journal = Journal::new(&dir, 2).unwrap();
        thread::scope(|scope| {
            for key in ["a", "b"] {
                let journal = &journal;
                scope.spawn(move || journal.start(key, key, "l6", "headless"));
            }
        });
        let read = || {
            serde_json::from_slice::<Snapshot>(&fs::read(dir.join("progress.json")).unwrap())
                .unwrap()
        };
        assert_eq!(read().active.len(), 2);
        journal.finish("a", Outcome::Flaky);
        journal.finish("b", Outcome::Pass);
        journal.complete("FLAKY").unwrap();
        let snapshot = read();
        assert_eq!(
            (snapshot.completed, snapshot.outcome.as_str()),
            (2, "FLAKY")
        );
        assert!(snapshot.active.is_empty());
        assert_eq!(snapshot.counts.get("FLAKY"), Some(&1));
        drop(journal);
        assert_eq!(read().outcome, "FLAKY");
        fs::remove_dir_all(dir).unwrap();
    }
    #[test]
    fn renders_concurrent_cases_and_all_outcomes() {
        let mut active = BTreeMap::new();
        active.insert(
            "a".into(),
            Active {
                case: "boot".into(),
                model: "l6".into(),
                backend: "gui".into(),
            },
        );
        active.insert(
            "b".into(),
            Active {
                case: "midi".into(),
                model: "l6max".into(),
                backend: "headless".into(),
            },
        );
        let state = Snapshot {
            total: 4,
            completed: 2,
            outcome: "RUNNING".into(),
            counts: [("FLAKY".into(), 1), ("XPASS".into(), 1)].into(),
            active,
        };
        let text = render(&state, false);
        for expected in [
            "2/4 (50%)",
            "1 flaky",
            "1 unexpected passes",
            "boot (l6, gui)",
            "midi (l6max, headless)",
        ] {
            assert!(text.contains(expected), "{text}");
        }
        assert!(!text.contains('\x1b'));
    }
}
