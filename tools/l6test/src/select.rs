//! Case selection beyond one suite: id globs, shards and reruns of a report.
use crate::report::Outcome;
use sha2::{Digest, Sha256};
use std::{collections::BTreeSet, path::Path};

/// Match `text` against a glob of `*` (any run) and `?` (any one character).
pub fn glob(pattern: &str, text: &str) -> bool {
    let (pattern, text): (Vec<char>, Vec<char>) =
        (pattern.chars().collect(), text.chars().collect());
    let (mut p, mut t) = (0, 0);
    let mut star: Option<(usize, usize)> = None;
    while t < text.len() {
        if p < pattern.len() && (pattern[p] == '?' || pattern[p] == text[t]) {
            p += 1;
            t += 1;
        } else if p < pattern.len() && pattern[p] == '*' {
            star = Some((p, t));
            p += 1;
        } else if let Some((star_p, star_t)) = star {
            p = star_p + 1;
            t = star_t + 1;
            star = Some((star_p, star_t + 1));
        } else {
            return false;
        }
    }
    pattern[p..].iter().all(|c| *c == '*')
}

/// Whether a case id matches a `--case` value: comma-separated ids or globs.
pub fn case_matches(selection: &str, id: &str) -> bool {
    selection.split(',').any(|pattern| glob(pattern.trim(), id))
}

/// Patterns of a `--case` value that match none of `ids`. A misspelled id
/// next to valid ones would otherwise be silently skipped.
pub fn unmatched<'a>(selection: &'a str, ids: &[&str]) -> Vec<&'a str> {
    selection
        .split(',')
        .map(str::trim)
        .filter(|pattern| !ids.iter().any(|id| glob(pattern, id)))
        .collect()
}

/// `--shard I/N`: run the I-th of N stable parts (1-based).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Shard {
    pub index: u64,
    pub count: u64,
}

impl Shard {
    pub fn parse(value: &str) -> Result<Self, String> {
        let (index, count) = value
            .split_once('/')
            .and_then(|(i, n)| Some((i.parse().ok()?, n.parse().ok()?)))
            .ok_or("--shard needs I/N, for example 1/3")?;
        if count == 0 || index == 0 || index > count {
            return Err("--shard needs 1 <= I <= N".into());
        }
        Ok(Self { index, count })
    }
    /// A job's shard depends only on its case, model and backend, so a shard
    /// keeps its cases when other cases are added or removed.
    pub fn contains(&self, case: &str, model: &str, backend: &str) -> bool {
        let digest = Sha256::digest(format!("{case}\0{model}\0{backend}").as_bytes());
        let value = u64::from_le_bytes(digest[..8].try_into().unwrap());
        value % self.count == self.index - 1
    }
}

/// The jobs of an earlier report to run again.
pub struct Rerun {
    jobs: BTreeSet<(String, String, String)>,
}

/// Outcomes `--rerun` selects unless `--only` names others.
pub const RERUN_DEFAULT: &[Outcome] = &[
    Outcome::Fail,
    Outcome::Xpass,
    Outcome::Blocked,
    Outcome::Flaky,
];

pub fn parse_outcomes(value: &str) -> Result<Vec<Outcome>, String> {
    value
        .split(',')
        .map(|name| {
            serde_json::from_value(serde_json::Value::String(name.trim().to_uppercase()))
                .map_err(|_| format!("unknown outcome {name}"))
        })
        .collect()
}

impl Rerun {
    /// Load `report` (a `report.json` or the run directory holding it).
    pub fn load(report: &Path, outcomes: &[Outcome]) -> Result<Self, String> {
        let path = if report.is_dir() {
            report.join("report.json")
        } else {
            report.to_owned()
        };
        let results = crate::report::read_results(&path)?;
        Ok(Self {
            jobs: results
                .into_iter()
                .filter(|r| outcomes.contains(&r.outcome))
                .map(|r| (r.case, r.model, r.backend))
                .collect(),
        })
    }
    pub fn has_case(&self, case: &str) -> bool {
        self.jobs.iter().any(|(id, _, _)| id == case)
    }
    pub fn contains(&self, case: &str, model: &str, backend: &str) -> bool {
        self.jobs
            .contains(&(case.to_owned(), model.to_owned(), backend.to_owned()))
    }
    pub fn is_empty(&self) -> bool {
        self.jobs.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn globs_match_ids() {
        assert!(glob("patches-*", "patches-l6max-sd-knob-dialog"));
        assert!(glob("*knob*", "sd-knob-dialog"));
        assert!(glob("l6?-boot", "l6x-boot"));
        assert!(glob("boot", "boot"));
        assert!(!glob("boot", "boot-extra"));
        assert!(!glob("patches-*-l6", "patches-l6max"));
        assert!(glob("*", ""));
        assert!(case_matches("boot,patches-*", "patches-l6"));
        assert!(!case_matches("boot,mute", "encoder"));
        assert_eq!(
            unmatched("boot,bot,patches-*", &["boot", "patches-l6"]),
            ["bot"]
        );
        assert!(unmatched("boot", &["boot"]).is_empty());
    }

    #[test]
    fn shards_partition_jobs_and_are_stable() {
        let shards: Vec<_> = (1..=3).map(|i| Shard { index: i, count: 3 }).collect();
        let jobs: Vec<String> = (0..300).map(|i| format!("case-{i}")).collect();
        let mut sizes = [0; 3];
        for job in &jobs {
            let owners: Vec<_> = shards
                .iter()
                .enumerate()
                .filter(|(_, s)| s.contains(job, "l6max", "headless"))
                .map(|(i, _)| i)
                .collect();
            assert_eq!(owners.len(), 1, "{job} belongs to exactly one shard");
            sizes[owners[0]] += 1;
        }
        assert!(sizes.iter().all(|size| *size > 60), "{sizes:?}");
        assert!(
            shards[0].contains("case-0", "l6max", "headless")
                == Shard::parse("1/3")
                    .unwrap()
                    .contains("case-0", "l6max", "headless")
        );
        assert!(Shard::parse("0/3").is_err());
        assert!(Shard::parse("4/3").is_err());
        assert!(Shard::parse("x").is_err());
    }

    #[test]
    fn rerun_selects_jobs_by_outcome_from_schema_1_and_2_reports() {
        let dir = std::env::temp_dir().join(format!("l6test-rerun-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        fs::write(
            dir.join("report.json"),
            r#"{"schema":1,"outcome":"FAIL","results":[
              {"case":"a","model":"l6","backend":"headless","outcome":"FAIL","detail":"","artifacts":"/x"},
              {"case":"b","model":"l6","backend":"headless","outcome":"PASS","detail":"","artifacts":"/x"},
              {"case":"c","model":"l6max","backend":"gui","outcome":"BLOCKED","detail":"","artifacts":"/x","phase":"preflight"}]}"#,
        )
        .unwrap();
        let rerun = Rerun::load(&dir, RERUN_DEFAULT).unwrap();
        assert!(rerun.contains("a", "l6", "headless"));
        assert!(!rerun.has_case("b"));
        assert!(rerun.contains("c", "l6max", "gui"));
        assert!(!rerun.contains("c", "l6max", "headless"));
        let only = Rerun::load(&dir, &parse_outcomes("pass").unwrap()).unwrap();
        assert!(only.has_case("b") && !only.has_case("a"));
        assert!(parse_outcomes("nope").is_err());
        fs::remove_dir_all(dir).unwrap();
    }
}
