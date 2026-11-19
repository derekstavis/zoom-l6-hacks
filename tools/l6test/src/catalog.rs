//! The case catalog: `tests/smoke/catalog.toml` plus generated cases, the run's
//! frozen copy of the spec, and the case matrix that selection expands.
use crate::patch_matrix;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use smoke_core::Scenario;
use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
};

#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ExpectedFailure {
    pub model: String,
    pub step: usize,
    pub contains: String,
    pub reason: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct File {
    version: u32,
    case: Vec<Case>,
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Case {
    /// Known failures at an exact step and error. A pass becomes XPASS.
    #[serde(default)]
    pub expected_failure: Vec<ExpectedFailure>,
    pub id: String,
    pub suites: Vec<String>,
    pub models: Vec<String>,
    pub scenario: Option<PathBuf>,
    pub package: Option<String>,
    #[serde(default)]
    pub arguments: Vec<String>,
    #[serde(default)]
    pub environment: BTreeMap<String, String>,
    #[serde(default)]
    pub needs_configuration: bool,
    #[serde(default)]
    pub gui: bool,
    #[serde(default)]
    pub volatile: bool,
    #[serde(default)]
    pub no_sd: bool,
    pub blocked: Option<String>,
    #[serde(default = "case_timeout")]
    pub wall_timeout_s: u64,
}

fn case_timeout() -> u64 {
    900
}

pub struct Catalog {
    pub cases: Vec<Case>,
    pub generated: Vec<patch_matrix::GeneratedCase>,
}

/// Load the catalog under `spec` (normally `tests/smoke`) and append the
/// generated cases. Generated scenarios exist only in text until
/// [`freeze`] writes them into a spec copy.
pub fn load(spec: &Path) -> Result<Catalog, String> {
    let file: File =
        toml::from_str(&fs::read_to_string(spec.join("catalog.toml")).map_err(|e| e.to_string())?)
            .map_err(|e| e.to_string())?;
    if file.version != 1 {
        return Err("unsupported catalog schema".into());
    }
    let mut cases = file.case;
    for case in &cases {
        if let Some(path) = &case.scenario {
            Scenario::load(&spec.join(path))?;
        }
    }
    let generated = patch_matrix::generate()?;
    for case in &generated {
        if cases.iter().any(|existing| existing.id == case.id) {
            return Err(format!(
                "generated case {} duplicates a catalog case",
                case.id
            ));
        }
        cases.push(Case {
            expected_failure: Vec::new(),
            id: case.id.clone(),
            suites: vec![patch_matrix::SUITE.into()],
            models: vec![case.model.into()],
            scenario: Some(PathBuf::from(format!("scenarios/{}.toml", case.id))),
            package: None,
            arguments: Vec::new(),
            environment: BTreeMap::new(),
            needs_configuration: true,
            gui: false,
            volatile: false,
            no_sd: false,
            blocked: None,
            wall_timeout_s: 1200,
        });
    }
    Ok(Catalog { cases, generated })
}

/// Copy the spec to `target` and write the generated scenarios into it,
/// validating each. Cases then run from this frozen copy.
pub fn freeze(spec: &Path, catalog: &Catalog, target: &Path) -> Result<(), String> {
    crate::copy_tree(spec, target)?;
    for case in &catalog.generated {
        let path = target.join(format!("scenarios/{}.toml", case.id));
        fs::write(&path, &case.scenario).map_err(|e| e.to_string())?;
        Scenario::load(&path).map_err(|error| format!("{}: {error}", case.id))?;
    }
    Ok(())
}

/// The backends a case runs on, given the requested ones. A `gui` scenario
/// case runs on the GUI whatever was requested; a case without a scenario runs
/// on its declared backend.
pub fn backends<'a>(
    case: &Case,
    scenario: Option<&Scenario>,
    requested: &[&'a str],
) -> Vec<&'a str> {
    match scenario {
        Some(_) if case.gui => vec!["gui"],
        Some(scenario) => requested
            .iter()
            .copied()
            .filter(|b| scenario.backends.iter().any(|s| s == b))
            .collect(),
        None => vec![if case.gui { "gui" } else { "headless" }],
    }
}

/// Every (case, model, backend) the catalog can run, with suite membership and,
/// for generated cases, the scenario text digest. `frozen` must hold the
/// generated scenarios (see [`freeze`]).
pub fn matrix(catalog: &Catalog, frozen: &Path) -> Result<Vec<Value>, String> {
    let generated: BTreeMap<&str, String> = catalog
        .generated
        .iter()
        .map(|case| {
            (
                case.id.as_str(),
                format!("{:x}", Sha256::digest(case.scenario.as_bytes())),
            )
        })
        .collect();
    let mut rows = Vec::new();
    for case in &catalog.cases {
        let scenario = case
            .scenario
            .as_ref()
            .map(|path| Scenario::load(&frozen.join(path)))
            .transpose()?;
        for model in &case.models {
            for backend in backends(case, scenario.as_ref(), &["headless", "gui"]) {
                let mut row = json!({
                    "case": case.id,
                    "model": model,
                    "backend": backend,
                    "suites": case.suites,
                });
                if let Some(digest) = generated.get(case.id.as_str()) {
                    row["generated_scenario_sha256"] = json!(digest);
                }
                rows.push(row);
            }
        }
    }
    Ok(rows)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The case matrix is reviewed: a change that adds, removes or moves cases
    /// between models, backends or suites, or changes a generated scenario,
    /// must update `snapshots/cases.json` deliberately. Regenerate it with
    /// `L6TEST_UPDATE_SNAPSHOT=1 cargo test -p l6test case_matrix`.
    #[test]
    fn case_matrix_matches_the_reviewed_snapshot() {
        let root = l6max_host::paths::repository_root();
        let spec = root.join("tests/smoke");
        let catalog = load(&spec).unwrap();
        let frozen = std::env::temp_dir().join(format!("l6test-matrix-{}", std::process::id()));
        let _ = fs::remove_dir_all(&frozen);
        freeze(&spec, &catalog, &frozen).unwrap();
        let rows = matrix(&catalog, &frozen).unwrap();
        fs::remove_dir_all(&frozen).unwrap();
        let actual = serde_json::to_string_pretty(&rows).unwrap() + "\n";
        let path = root.join("tools/l6test/snapshots/cases.json");
        if std::env::var_os("L6TEST_UPDATE_SNAPSHOT").is_some() {
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(&path, &actual).unwrap();
            return;
        }
        let expected = fs::read_to_string(&path).unwrap_or_default();
        if actual != expected {
            let expected: Vec<Value> = serde_json::from_str(&expected).unwrap_or_default();
            let key = |row: &Value| row.to_string();
            let before: std::collections::BTreeSet<_> = expected.iter().map(key).collect();
            let after: std::collections::BTreeSet<_> = rows.iter().map(key).collect();
            let removed: Vec<_> = before.difference(&after).take(10).collect();
            let added: Vec<_> = after.difference(&before).take(10).collect();
            panic!(
                "the case matrix changed; review it and regenerate with \
                 L6TEST_UPDATE_SNAPSHOT=1 cargo test -p l6test case_matrix\n\
                 removed (first 10): {removed:#?}\nadded (first 10): {added:#?}"
            );
        }
    }
}
