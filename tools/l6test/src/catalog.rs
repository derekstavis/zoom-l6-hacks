//! The case catalog: `tests/smoke/catalog.toml` plus generated cases, and the
//! case matrix that selection expands.
use crate::patch_matrix;
use serde::{Deserialize, Serialize};
use serde_json::json;
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
    /// Every case's scenario, by case id. Generated scenarios are parsed from
    /// their text as if they sat in the spec's `scenarios/` directory.
    pub scenarios: BTreeMap<String, Scenario>,
}

/// Load the catalog under `spec` (normally `tests/smoke`), append the
/// generated cases, and parse and validate every scenario. Generated scenarios
/// exist only as text until [`write_generated`] writes them into a run.
pub fn load(spec: &Path) -> Result<Catalog, String> {
    let file: File =
        toml::from_str(&fs::read_to_string(spec.join("catalog.toml")).map_err(|e| e.to_string())?)
            .map_err(|e| e.to_string())?;
    if file.version != 1 {
        return Err("unsupported catalog schema".into());
    }
    let mut cases = file.case;
    let mut scenarios = BTreeMap::new();
    for case in &cases {
        if let Some(path) = &case.scenario {
            let scenario = Scenario::load(&spec.join(path))
                .map_err(|error| format!("{}: {error}", case.id))?;
            scenarios.insert(case.id.clone(), scenario);
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
    for case in &generated {
        let path = spec.join(format!("scenarios/{}.toml", case.id));
        let scenario = Scenario::from_text(&case.scenario, &path)
            .map_err(|error| format!("{}: {error}", case.id))?;
        scenarios.insert(case.id.clone(), scenario);
    }
    Ok(Catalog {
        cases,
        generated,
        scenarios,
    })
}

/// Write the generated scenarios into `dir` (a run's `generated/`), point
/// their cases at those files, and reload them so their sources are files the
/// case processes can read. Hand-written scenarios run from the spec in place.
pub fn write_generated(catalog: &mut Catalog, dir: &Path) -> Result<(), String> {
    fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    for generated in &catalog.generated {
        let path = dir.join(format!("{}.toml", generated.id));
        fs::write(&path, &generated.scenario).map_err(|e| e.to_string())?;
        let scenario =
            Scenario::load(&path).map_err(|error| format!("{}: {error}", generated.id))?;
        catalog.scenarios.insert(generated.id.clone(), scenario);
        if let Some(case) = catalog.cases.iter_mut().find(|c| c.id == generated.id) {
            case.scenario = Some(path);
        }
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

/// One (case, model, backend) the catalog can run. Fields serialize in this
/// order whatever `serde_json` features a build enables.
#[derive(Debug, Deserialize, PartialEq, Serialize)]
pub struct Row {
    pub case: String,
    pub model: String,
    pub backend: String,
    pub suites: Vec<String>,
    /// Digest of what the case runs for this model, independent of where its
    /// files live: scenario and fragment contents, behavioral catalog fields,
    /// and the contents of its state and card specs.
    pub definition: String,
}

/// Digest of what `case` runs for `model`; see [`Row::definition`].
/// `generated` is the text of a generated scenario, whose file does not exist.
fn definition(
    case: &Case,
    scenario: Option<&Scenario>,
    generated: Option<&str>,
    model: &str,
    spec: &Path,
) -> Result<String, String> {
    let digest = |bytes: &[u8]| format!("{:x}", Sha256::digest(bytes));
    let content = |path: &Path| -> Result<String, String> {
        Ok(digest(
            &fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?,
        ))
    };
    let spec_file = |directory: &str, name: &Option<String>| -> Result<Option<String>, String> {
        name.as_ref()
            .map(|name| content(&spec.join(directory).join(format!("{model}-{name}.toml"))))
            .transpose()
    };
    let (scenario_files, state, card) = match scenario {
        Some(scenario) => (
            scenario
                .sources
                .iter()
                .enumerate()
                .map(|(index, path)| match (index, generated) {
                    (0, Some(text)) => Ok(digest(text.as_bytes())),
                    _ => content(path),
                })
                .collect::<Result<Vec<_>, _>>()?,
            spec_file("states", &scenario.state)?,
            spec_file("cards", &scenario.card)?,
        ),
        None => (Vec::new(), None, None),
    };
    Ok(crate::hashing::digest(&json!({
        "scenario_files": scenario_files,
        "state": state,
        "card": card,
        "package": case.package,
        "arguments": case.arguments,
        "environment": case.environment,
        "needs_configuration": case.needs_configuration,
        "gui": case.gui,
        "volatile": case.volatile,
        "no_sd": case.no_sd,
        "blocked": case.blocked,
        "wall_timeout_s": case.wall_timeout_s,
        "expected_failure": case.expected_failure.iter().map(|e| json!([e.model, e.step, e.contains])).collect::<Vec<_>>(),
    })))
}

/// Every (case, model, backend) the catalog under `spec` can run, sorted.
pub fn matrix(catalog: &Catalog, spec: &Path) -> Result<Vec<Row>, String> {
    let generated: BTreeMap<&str, &str> = catalog
        .generated
        .iter()
        .map(|case| (case.id.as_str(), case.scenario.as_str()))
        .collect();
    let mut rows = Vec::new();
    for case in &catalog.cases {
        let scenario = catalog.scenarios.get(&case.id);
        for model in &case.models {
            let definition = definition(
                case,
                scenario,
                generated.get(case.id.as_str()).copied(),
                model,
                spec,
            )
            .map_err(|error| format!("{}: {error}", case.id))?;
            for backend in backends(case, scenario, &["headless", "gui"]) {
                rows.push(Row {
                    case: case.id.clone(),
                    model: model.clone(),
                    backend: backend.into(),
                    suites: case.suites.clone(),
                    definition: definition.clone(),
                });
            }
        }
    }
    rows.sort_by(|a, b| (&a.case, &a.model, &a.backend).cmp(&(&b.case, &b.model, &b.backend)));
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
        let rows = matrix(&catalog, &spec).unwrap();
        let actual = serde_json::to_string_pretty(&rows).unwrap() + "\n";
        let path = root.join("tools/l6test/snapshots/cases.json");
        if std::env::var_os("L6TEST_UPDATE_SNAPSHOT").is_some() {
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(&path, &actual).unwrap();
            return;
        }
        let expected = fs::read_to_string(&path).unwrap_or_default();
        if actual != expected {
            let expected: Vec<Row> = serde_json::from_str(&expected).unwrap_or_default();
            let key = |row: &Row| serde_json::to_string(row).unwrap();
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
