//! Versioned, append-only live progress events shared by the driver and runners.
use serde::{Deserialize, Serialize};
use std::{
    fs::{File, OpenOptions},
    io::{self, Write},
    path::{Path, PathBuf},
};

pub const VERSION: u32 = 1;
pub const PATH_ENV: &str = "L6_TEST_PROGRESS";
pub const RUN_ENV: &str = "L6_TEST_PROGRESS_RUN";
pub const ATTEMPT_ENV: &str = "L6_TEST_PROGRESS_ATTEMPT";

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct CaseId {
    pub case: String,
    pub model: String,
    pub backend: String,
}
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct AttemptId {
    pub case: CaseId,
    pub attempt: u32,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum Outcome {
    Pass,
    Fail,
    Flaky,
    Blocked,
    Xfail,
    Xpass,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Phase {
    PreparingFixture,
    Executing,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum SuiteEvent {
    Header {
        version: u32,
        run_id: String,
    },
    RunStarted {
        total_cases: usize,
    },
    AttemptStarted {
        id: AttemptId,
        progress_path: PathBuf,
    },
    AttemptPhaseChanged {
        id: AttemptId,
        phase: Phase,
    },
    AttemptFinished {
        id: AttemptId,
        outcome: Outcome,
    },
    CaseFinished {
        id: CaseId,
        outcome: Outcome,
    },
    RunFinished {
        outcome: Outcome,
    },
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum CaseEvent {
    Header {
        version: u32,
        run_id: String,
        id: AttemptId,
    },
    ScenarioStarted {
        total_steps: usize,
    },
    StepStarted {
        index: usize,
        label: String,
    },
    ScenarioFinished {
        outcome: Outcome,
    },
}

pub struct Writer {
    file: File,
}
impl Writer {
    pub fn create(path: &Path) -> io::Result<Self> {
        Ok(Self {
            file: OpenOptions::new().write(true).create_new(true).open(path)?,
        })
    }
    pub fn emit<T: Serialize>(&mut self, event: &T) -> io::Result<()> {
        let mut line = serde_json::to_vec(event).map_err(io::Error::other)?;
        line.push(b'\n');
        self.file.write_all(&line)?;
        self.file.flush()
    }
}
