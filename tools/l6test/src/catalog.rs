//! The case catalog: every case file under `tests/emulator/cases` and each
//! patch's `tests/emulator/cases`, plus generated cases, and the case matrix
//! that selection expands.
//!
//! A case file is a scenario with a `[case]` table saying how the driver runs
//! it. A file with only a `[case]` table either names another case's scenario
//! (`scenario`, to run it differently) or, with neither, drives the packaged
//! application. A case's id is its file name.
use crate::patch_matrix;
use l6fw::Model;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use smoke_core::{Backend, Scenario};
use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
};

/// The shared emulator test spec, from the repository root.
pub const SPEC: &str = "tests/emulator";
/// The suites cases may name, from the repository root.
pub const SUITES_FILE: &str = "tests/emulator/suites.toml";

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SuitesFile {
    version: u32,
    suite: BTreeMap<String, SuiteEntry>,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SuiteEntry {
    pub description: String,
    /// Other suites whose cases this one runs too.
    #[serde(default)]
    pub includes: Vec<String>,
}

/// The declared suites.
pub struct Suites {
    pub declared: BTreeMap<String, SuiteEntry>,
}

impl Suites {
    pub(crate) fn load(root: &Path) -> Result<Self, String> {
        let path = root.join(SUITES_FILE);
        let file: SuitesFile =
            toml::from_str(&fs::read_to_string(&path).map_err(|e| format!("{SUITES_FILE}: {e}"))?)
                .map_err(|e| format!("{SUITES_FILE}: {e}"))?;
        if file.version != 1 {
            return Err(format!("{SUITES_FILE}: unsupported version"));
        }
        let suites = Self {
            declared: file.suite,
        };
        for name in suites.declared.keys() {
            suites.members(name)?;
        }
        Ok(suites)
    }

    /// `name` and every suite it includes, directly or through others.
    pub fn members(&self, name: &str) -> Result<Vec<String>, String> {
        let mut members = Vec::new();
        let mut pending = vec![(name.to_owned(), 0)];
        while let Some((suite, depth)) = pending.pop() {
            if depth > self.declared.len() {
                return Err(format!("{SUITES_FILE}: suite {name} includes itself"));
            }
            let entry = self
                .declared
                .get(&suite)
                .ok_or_else(|| format!("unknown suite {suite}; see {SUITES_FILE}"))?;
            if !members.contains(&suite) {
                members.push(suite.clone());
            }
            pending.extend(entry.includes.iter().map(|s| (s.clone(), depth + 1)));
        }
        Ok(members)
    }
}
/// A patch's emulator tests, from its directory.
pub const PATCH_SPEC: &str = "tests/emulator";

#[derive(Clone, Deserialize, Serialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ExpectedFailure {
    pub model: Model,
    pub step: usize,
    pub contains: String,
    pub reason: String,
}

/// A case file's `[case]` table: how the test driver runs the file's
/// scenario, or another case's.
#[derive(Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct Header {
    /// Another case's scenario, for a file without one of its own.
    scenario: Option<PathBuf>,
    #[serde(default)]
    campaign: Option<Vec<HeaderCampaignMember>>,
    /// The models of a case without a scenario of its own; otherwise the
    /// scenario's `models` decide.
    models: Option<Vec<Model>>,
    #[serde(default)]
    suites: Vec<String>,
    #[serde(default)]
    expected_failure: Vec<ExpectedFailure>,
    #[serde(default)]
    environment: BTreeMap<String, String>,
    #[serde(default)]
    gui: bool,
    #[serde(default)]
    volatile: bool,
    #[serde(default)]
    no_sd: bool,
    blocked: Option<String>,
    #[serde(default = "case_timeout")]
    wall_timeout_s: u64,
}

#[derive(Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct HeaderCampaignMember {
    name: String,
    model: Model,
    scenario: PathBuf,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CampaignMember {
    pub name: String,
    pub model: Model,
    pub scenario: PathBuf,
    pub fingerprint: String,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct Case {
    pub campaign: Vec<CampaignMember>,
    /// Known failures at an exact step and error. A pass becomes XPASS.
    pub expected_failure: Vec<ExpectedFailure>,
    pub id: String,
    pub suites: Vec<String>,
    pub models: Vec<Model>,
    /// The scenario file, from the repository root (absolute for generated
    /// cases). None for a case that drives the packaged application.
    pub scenario: Option<PathBuf>,
    pub environment: BTreeMap<String, String>,
    pub gui: bool,
    pub volatile: bool,
    pub no_sd: bool,
    pub blocked: Option<String>,
    pub wall_timeout_s: u64,
}

fn case_timeout() -> u64 {
    900
}

pub struct Catalog {
    pub cases: Vec<Case>,
    pub suites: Suites,
    pub generated: Vec<patch_matrix::GeneratedCase>,
    /// Every case's scenario, by case id.
    pub scenarios: BTreeMap<String, Scenario>,
}

/// Every directory holding emulator test files: the shared spec, then each
/// patch's tests in name order.
pub fn spec_dirs(root: &Path) -> Result<Vec<PathBuf>, String> {
    let mut dirs = vec![root.join(SPEC)];
    let mut patches = fs::read_dir(root.join("patches"))
        .map_err(|e| e.to_string())?
        .map(|entry| entry.map(|e| e.path().join(PATCH_SPEC)))
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| e.to_string())?;
    patches.sort();
    dirs.extend(patches.into_iter().filter(|dir| dir.is_dir()));
    Ok(dirs)
}

/// A state spec for `model`, by the name a scenario gives it.
pub fn state_spec(model: Model, name: &str) -> PathBuf {
    l6max_host::paths::repository_root().join(format!("{SPEC}/fixtures/states/{model}-{name}.toml"))
}

/// A card spec, by the name a scenario gives it. Card specs serve both models;
/// load one with `CardSpec::load_for`.
pub fn card_spec(name: &str) -> PathBuf {
    l6max_host::paths::repository_root().join(format!("{SPEC}/fixtures/cards/{name}.toml"))
}

fn toml_files(dir: &Path, out: &mut Vec<PathBuf>) -> Result<(), String> {
    for entry in fs::read_dir(dir).map_err(|e| format!("{}: {e}", dir.display()))? {
        let path = entry.map_err(|e| e.to_string())?.path();
        if path.is_dir() {
            toml_files(&path, out)?;
        } else if path.extension().is_some_and(|e| e == "toml") {
            out.push(path);
        }
    }
    Ok(())
}

/// Read one case file, given from the repository root.
fn load_case(root: &Path, file: &Path) -> Result<(Case, Option<Scenario>), String> {
    let id = file
        .file_stem()
        .and_then(|stem| stem.to_str())
        .ok_or("case file has no name")?
        .to_owned();
    let mut table: toml::Table =
        toml::from_str(&fs::read_to_string(root.join(file)).map_err(|e| e.to_string())?)
            .map_err(|e| e.to_string())?;
    let header: Header = table
        .remove("case")
        .ok_or("a case file needs a [case] table")?
        .try_into()
        .map_err(|e: toml::de::Error| format!("[case]: {e}"))?;
    let (scenario_path, models) = match (table.is_empty(), header.scenario, header.models) {
        (false, None, None) => (Some(file.to_owned()), None),
        (false, _, _) => {
            return Err(
                "a case with its own scenario takes its models from it and names no other scenario"
                    .into(),
            );
        }
        (true, scenario, Some(models)) => (scenario, Some(models)),
        (true, _, None) => return Err("a case without its own scenario needs models".into()),
    };
    let scenario = scenario_path
        .as_ref()
        .map(|path| Scenario::load(&scenario_file(path)))
        .transpose()?;
    let models = match (models, &scenario) {
        (Some(models), _) => models,
        (None, Some(scenario)) => scenario.models.clone(),
        (None, None) => unreachable!(),
    };
    if header.campaign.as_ref().is_some_and(Vec::is_empty) {
        return Err("campaign must have at least one member".into());
    }
    if header.campaign.is_some()
        && (!table.is_empty()
            || scenario_path.is_some()
            || header.gui
            || header.volatile
            || header.no_sd
            || !header.expected_failure.is_empty())
    {
        return Err(
            "campaign cases require only model-scoped members and isolated headless fixtures"
                .into(),
        );
    }
    let mut case = Case {
        campaign: header
            .campaign
            .unwrap_or_default()
            .into_iter()
            .map(|member| CampaignMember {
                name: member.name,
                model: member.model,
                scenario: member.scenario,
                fingerprint: String::new(),
            })
            .collect(),
        expected_failure: header.expected_failure,
        id,
        suites: header.suites,
        models,
        scenario: scenario_path,
        environment: header.environment,
        gui: header.gui,
        volatile: header.volatile,
        no_sd: header.no_sd,
        blocked: header.blocked,
        wall_timeout_s: header.wall_timeout_s,
    };
    validate_campaign(&mut case.campaign, &case.models, &case.id)?;
    Ok((case, scenario))
}

fn validate_campaign(
    members: &mut [CampaignMember],
    models: &[Model],
    id: &str,
) -> Result<(), String> {
    if members.is_empty() {
        return Ok(());
    }
    if models.is_empty()
        || models
            .iter()
            .collect::<std::collections::BTreeSet<_>>()
            .len()
            != models.len()
    {
        return Err(format!("{id}: campaign needs distinct models"));
    }
    let mut names = std::collections::BTreeSet::new();
    for member in members.iter_mut() {
        if member.name.is_empty()
            || !member
                .name
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
            || !names.insert((member.model, member.name.clone()))
            || !models.contains(&member.model)
        {
            return Err(format!("{id} has an invalid or duplicate campaign member"));
        }
        let scenario = Scenario::load(&scenario_file(&member.scenario))?;
        if !scenario.models.contains(&member.model)
            || !scenario.backends.contains(&Backend::Headless)
        {
            return Err(format!(
                "{id}: campaign member {} is incompatible",
                member.name
            ));
        }
        member.fingerprint = crate::hashing::digest(&resolved_scenario(&scenario, member.model)?);
    }
    if models
        .iter()
        .any(|model| !members.iter().any(|member| member.model == *model))
    {
        return Err(format!("{id}: campaign has a model without members"));
    }
    Ok(())
}

/// Load every case under `root` (the repository), append the generated
/// cases, and parse and validate every scenario. Generated scenarios exist
/// only as text until [`write_generated`] writes them into a run.
pub fn load(root: &Path) -> Result<Catalog, String> {
    let mut files = Vec::new();
    for dir in spec_dirs(root)? {
        let cases = dir.join("cases");
        if cases.is_dir() {
            let mut found = Vec::new();
            toml_files(&cases, &mut found)?;
            found.sort();
            files.extend(found);
        }
    }
    let mut cases: Vec<Case> = Vec::new();
    let mut origins = BTreeMap::new();
    let mut scenarios = BTreeMap::new();
    for path in files {
        let file = path.strip_prefix(root).unwrap().to_owned();
        let (case, scenario) =
            load_case(root, &file).map_err(|error| format!("{}: {error}", file.display()))?;
        if let Some(other) = origins.insert(case.id.clone(), file.clone()) {
            return Err(format!(
                "case id {} is defined by both {} and {}",
                case.id,
                other.display(),
                file.display()
            ));
        }
        if let Some(scenario) = scenario {
            scenarios.insert(case.id.clone(), scenario);
        }
        cases.push(case);
    }
    let mut generated = patch_matrix::generate()?;
    for case in &mut generated {
        validate_campaign(&mut case.campaign, &[case.model.parse()?], &case.id)?;
        if origins.contains_key(&case.id) {
            return Err(format!("generated case {} duplicates a case file", case.id));
        }
        cases.push(Case {
            campaign: case.campaign.clone(),
            expected_failure: Vec::new(),
            id: case.id.clone(),
            suites: case.suites.iter().map(|suite| (*suite).into()).collect(),
            models: vec![case.model.parse()?],
            // Replaced by the written file in `write_generated`.
            scenario: Some(PathBuf::from(format!("generated/{}.toml", case.id))),
            environment: BTreeMap::new(),
            gui: false,
            volatile: false,
            no_sd: false,
            blocked: None,
            wall_timeout_s: 1200,
        });
        let scenario =
            Scenario::from_text(&case.scenario).map_err(|error| format!("{}: {error}", case.id))?;
        scenarios.insert(case.id.clone(), scenario);
    }
    let suites = Suites::load(root)?;
    for case in &cases {
        for suite in &case.suites {
            if !suites.declared.contains_key(suite) {
                return Err(format!(
                    "case {} names suite {suite}, which {SUITES_FILE} does not declare",
                    case.id
                ));
            }
        }
    }
    Ok(Catalog {
        cases,
        suites,
        generated,
        scenarios,
    })
}

/// A case's scenario file. Catalog paths are relative to the repository
/// root; generated cases point at absolute files in their run.
pub fn scenario_file(path: &Path) -> PathBuf {
    l6max_host::paths::repository_root().join(path)
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
pub fn backends(case: &Case, scenario: Option<&Scenario>, requested: &[Backend]) -> Vec<Backend> {
    if !case.campaign.is_empty() {
        return requested
            .iter()
            .copied()
            .filter(|backend| *backend == Backend::Headless)
            .collect();
    }
    match scenario {
        Some(_) if case.gui => vec![Backend::Gui],
        Some(scenario) => requested
            .iter()
            .copied()
            .filter(|backend| scenario.backends.contains(backend))
            .collect(),
        None => vec![if case.gui {
            Backend::Gui
        } else {
            Backend::Headless
        }],
    }
}

/// One (case, model, backend) the catalog can run. Fields serialize in this
/// order whatever `serde_json` features a build enables.
#[derive(Debug, Deserialize, PartialEq, Serialize)]
pub struct Row {
    pub case: String,
    pub model: Model,
    pub backend: Backend,
    pub suites: Vec<String>,
    /// Digest of what the case runs for this model, independent of where its
    /// files live and how they are composed: the scenario's steps for this
    /// model after includes, its reference images, its state and card specs
    /// as data, and the behavioral catalog fields.
    pub definition: String,
}

/// Digest of what `case` runs for `model`; see [`Row::definition`].
fn definition(case: &Case, scenario: Option<&Scenario>, model: Model) -> Result<String, String> {
    let (scenario, state, card) = match scenario {
        Some(scenario) => (
            Some(resolved_scenario(scenario, model)?),
            scenario
                .state
                .as_ref()
                .map(|name| {
                    let path = state_spec(model, name);
                    let spec = l6nor::StateSpec::load(&path)?;
                    serde_json::to_value(spec).map_err(|e| e.to_string())
                })
                .transpose()?,
            scenario
                .card
                .as_ref()
                .map(|name| {
                    let spec = sd_image::spec::CardSpec::load_for(&card_spec(name), model.name())?;
                    serde_json::to_value(spec).map_err(|e| e.to_string())
                })
                .transpose()?,
        ),
        None => (None, None, None),
    };
    let mut value = json!({
        "scenario": scenario,
        "state": state,
        "card": card,
        "environment": case.environment,
        "gui": case.gui,
        "volatile": case.volatile,
        "no_sd": case.no_sd,
        "blocked": case.blocked,
        "wall_timeout_s": case.wall_timeout_s,
        "expected_failure": case
            .expected_failure
            .iter()
            .filter(|e| e.model == model)
            .map(|e| json!([e.step, e.contains]))
            .collect::<Vec<_>>(),
    });
    if !case.campaign.is_empty() {
        let members = case
            .campaign
            .iter()
            .filter(|member| member.model == model)
            .map(|member| {
                Scenario::load(&scenario_file(&member.scenario))
                    .and_then(|scenario| resolved_scenario(&scenario, model))
                    .map(|scenario| json!({"name": member.name, "scenario": scenario}))
            })
            .collect::<Result<Vec<_>, String>>()?;
        value["campaign"] = json!(members);
    }
    Ok(crate::hashing::digest(&value))
}

/// What a scenario runs on `model`, independent of where its files live and
/// how they are composed: its steps after includes, kept to those for `model`,
/// without its name or model list, and with each reference image replaced by
/// its content hash.
pub(crate) fn resolved_scenario(scenario: &Scenario, model: Model) -> Result<Value, String> {
    let mut value = serde_json::to_value(scenario).map_err(|e| e.to_string())?;
    let object = value.as_object_mut().unwrap();
    object.remove("name");
    object.remove("models");
    let steps = object["step"].as_array_mut().unwrap();
    steps.retain(|step| {
        step["models"]
            .as_array()
            .is_none_or(|models| models.is_empty() || models.iter().any(|m| m == model.name()))
    });
    for step in steps.iter_mut() {
        let step = step.as_object_mut().unwrap();
        step.remove("models");
        if let Some(Value::String(image)) = step.get("image") {
            // A reference not recorded yet fails its check when the case runs;
            // `--update-references` records it.
            let path = smoke_core::spec_path(Path::new(image))?;
            let identity = match fs::read(&path) {
                Ok(bytes) => format!("sha256:{:x}", Sha256::digest(bytes)),
                Err(_) => format!("missing:{image}"),
            };
            step.insert("image".into(), identity.into());
        }
    }
    Ok(value)
}

/// Every (case, model, backend) the catalog can run, sorted.
pub fn matrix(catalog: &Catalog) -> Result<Vec<Row>, String> {
    let mut rows = Vec::new();
    for case in &catalog.cases {
        let scenario = catalog.scenarios.get(&case.id);
        for model in &case.models {
            let definition = definition(case, scenario, *model)
                .map_err(|error| format!("{}: {error}", case.id))?;
            for backend in backends(case, scenario, &[Backend::Headless, Backend::Gui]) {
                rows.push(Row {
                    case: case.id.clone(),
                    model: *model,
                    backend,
                    suites: case.suites.clone(),
                    definition: definition.clone(),
                });
            }
        }
    }
    // By name, so the order matches the serialized rows.
    rows.sort_by(|a, b| {
        (&a.case, a.model.name(), a.backend.name()).cmp(&(
            &b.case,
            b.model.name(),
            b.backend.name(),
        ))
    });
    Ok(rows)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recorder_active_din_cases_only_skip_redundant_prepared_boot_wait() {
        use smoke_core::Action;
        let root = l6max_host::paths::repository_root();
        for path in [
            "tests/emulator/cases/patching/sd-all-patches.toml",
            "tests/emulator/cases/patching/sd-mixed-static.toml",
            "patches/din-mixer-control/tests/emulator/cases/sd-din-mixer-control.toml",
        ] {
            let source = fs::read_to_string(root.join(path)).unwrap();
            let old = source.replace("l6max-din-mixer-usb.toml", "l6max-din-mixer-setup.toml");
            assert_ne!(source, old, "{path}");
            let actual = Scenario::from_text(&source)
                .unwrap()
                .without_setup_input()
                .unwrap();
            let mut previous = Scenario::from_text(&old)
                .unwrap()
                .without_setup_input()
                .unwrap();
            let usb = previous
                .step
                .iter()
                .position(|step| matches!(step.action, Action::UsbControllerReady))
                .unwrap();
            assert!(
                matches!(&previous.step[usb-1].action, Action::Wait { until } if until == "boot-ready"),
                "{path}"
            );
            previous.step.remove(usb - 1);
            assert_eq!(
                serde_json::to_value(&actual.step).unwrap(),
                serde_json::to_value(&previous.step).unwrap(),
                "{path}"
            );
            let record = actual.step.iter().position(|step| matches!(&step.action, Action::Press { control } if control == "rec-record")).unwrap();
            let descriptors = actual
                .step
                .iter()
                .position(|step| matches!(step.action, Action::UsbEnumerate))
                .unwrap();
            assert!(record < descriptors, "{path}");
            assert!(actual.step[..record].iter().any(|step| matches!(&step.action, Action::ExpectDisplayImage { image, .. } if image == "tests/emulator/references/l6max-recorder-home.png")), "{path}");
            let usb = &actual.step[descriptors..descriptors + 6];
            assert_eq!(
                (usb[0].guest_timeout_ms, usb[0].wall_timeout_s),
                (60_000, 300)
            );
            assert!(matches!(usb[1].action, Action::UsbSysexBegin));
            assert_eq!(
                (usb[1].guest_timeout_ms, usb[1].wall_timeout_s),
                (15_000, 120)
            );
            let requests: Vec<_> = usb[2..5]
                .iter()
                .map(|step| match &step.action {
                    Action::UsbSysexExchange { request, .. } => (
                        request.bytes.clone(),
                        request.expect_prefix.clone(),
                        request.min_reply_bytes,
                        request.expect_reply,
                    ),
                    _ => panic!("missing USB SysEx exchange in {path}"),
                })
                .collect();
            assert_eq!(
                requests,
                vec![
                    (
                        vec![240, 126, 0, 6, 1, 247],
                        Some(vec![240, 126]),
                        None,
                        false
                    ),
                    (
                        vec![240, 82, 0, 0, 43, 247],
                        Some(vec![240, 82, 0, 0, 42]),
                        Some(182),
                        false
                    ),
                    (vec![240, 82, 0, 0, 49, 3, 1, 247], None, None, true),
                ],
                "{path}"
            );
            assert!(matches!(usb[5].action, Action::UsbSysexEnd));
        }
    }

    #[test]
    fn static_update_direct_selection_keeps_model_scoped_order() {
        use smoke_core::UpdateFixture::*;
        let catalog = load(l6max_host::paths::repository_root()).unwrap();
        for (id, model, expected, wall) in [
            ("firmware-update", Model::L6, vec![L6Matching], 1200),
            (
                "firmware-update",
                Model::L6max,
                vec![BadChecksum, SyntheticValid],
                1200,
            ),
            ("patched-update", Model::L6, vec![L6PatchedMidi], 900),
            (
                "patched-update",
                Model::L6max,
                vec![BadChecksumStock, PatchedKnob],
                900,
            ),
            (
                "patched-update-knob-midi",
                Model::L6max,
                vec![BadChecksumStock, PatchedKnobMidi],
                1200,
            ),
        ] {
            let case = catalog.cases.iter().find(|case| case.id == id).unwrap();
            let actual = case
                .campaign
                .iter()
                .filter(|member| member.model == model)
                .map(|member| {
                    Scenario::load(&scenario_file(&member.scenario))
                        .unwrap()
                        .update_fixture
                        .unwrap()
                })
                .collect::<Vec<_>>();
            assert_eq!(actual, expected, "{id}/{model}");
            assert_eq!(case.wall_timeout_s, wall);
            assert_eq!(backends(case, None, &[Backend::Gui]), Vec::<Backend>::new());
        }
    }

    #[test]
    fn static_campaign_rejects_empty_duplicate_and_wrong_model_members() {
        let root = l6max_host::paths::repository_root();
        let file = std::env::temp_dir().join(format!(
            "l6test-campaign-header-{}.toml",
            std::process::id()
        ));
        let prefix = "[case]\nmodels=['l6', 'l6max']\nsuites=['full']\n";
        let l6 = "[[case.campaign]]\nname='l6'\nmodel='l6'\nscenario='tests/emulator/cases/boot/l6-update-stock-install.toml'\n";
        let l6max = "[[case.campaign]]\nname='l6max'\nmodel='l6max'\nscenario='tests/emulator/cases/boot/l6max-update-bad-checksum.toml'\n";
        for source in [
            format!("{prefix}campaign=[]\n"),
            format!("{prefix}{l6}"),
            format!("{prefix}{l6}{l6}{l6max}"),
            format!(
                "{prefix}{}{}",
                l6.replace("model='l6'", "model='l6max'"),
                l6max
            ),
        ] {
            fs::write(&file, source).unwrap();
            assert!(load_case(root, &file).is_err());
        }
        fs::write(&file, format!("{prefix}{l6}{l6max}")).unwrap();
        assert!(load_case(root, &file).is_ok());
        fs::remove_file(file).unwrap();
    }

    /// The case matrix is reviewed: a change that adds, removes or moves cases
    /// between models, backends or suites, or changes a generated scenario,
    /// must update `snapshots/cases.json` deliberately. Regenerate it with
    /// `L6TEST_UPDATE_SNAPSHOT=1 cargo test -p l6test case_matrix`.
    #[test]
    fn case_matrix_matches_the_reviewed_snapshot() {
        let root = l6max_host::paths::repository_root();
        let catalog = load(&root).unwrap();
        let rows = matrix(&catalog).unwrap();
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
