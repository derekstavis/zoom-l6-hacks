//! Case identities, and reuse of earlier results with identical inputs.
//!
//! A case's identity hashes everything that decides its result: QEMU, the code
//! that runs and asserts the scenario, the firmware, patched images and SD
//! bundle the case boots, its card and state, its scenario files, the reference
//! images assertions read, audio input, the exact invocation and the case's
//! catalog entry. Fixtures are hashed as built bytes, not as the specs or code
//! that produced them, so a change to the driver alone keeps every key.
//!
//! `--reuse RUN` reports a case as REUSED, without running it, when RUN passed
//! (or expectedly failed) a case with the same key.
use crate::report::{CaseResult, Outcome, Reused};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet, HashMap},
    fs,
    path::{Path, PathBuf},
    process::Command,
};

pub fn digest(value: &Value) -> String {
    format!("{:x}", Sha256::digest(value.to_string().as_bytes()))
}

/// Digest of files by name and content, independent of where they live.
pub fn digest_files(files: &[(String, PathBuf)]) -> Result<String, String> {
    let mut sha = Sha256::new();
    for (name, path) in files {
        sha.update((name.len() as u64).to_le_bytes());
        sha.update(name.as_bytes());
        sha.update(crate::hash(path)?.as_bytes());
    }
    Ok(format!("{:x}", sha.finalize()))
}

/// Digest of a scenario's files, named relative to the spec root so the same
/// scenario in two run directories has the same digest.
pub fn scenario_digest(spec_root: &Path, sources: &[PathBuf]) -> Result<String, String> {
    let root = fs::canonicalize(spec_root).map_err(|e| e.to_string())?;
    let files = sources
        .iter()
        .map(|path| {
            let name = path
                .strip_prefix(&root)
                .unwrap_or(path)
                .to_string_lossy()
                .into_owned();
            (name, path.clone())
        })
        .collect::<Vec<_>>();
    digest_files(&files)
}

/// Digest of every file under `root`, or of nothing if it does not exist.
pub fn tree_digest(root: &Path) -> Result<String, String> {
    if !root.exists() {
        return Ok(digest(&Value::Null));
    }
    Ok(digest(&json!(crate::files(root)?)))
}

/// Identity of the code that runs a scenario.
///
/// The headless runner is the driver binary itself, which also links
/// `smoke-core`. When the driver built its executables from the working tree,
/// the runner is identified by the sources of `smoke-core` and every workspace
/// crate it depends on, plus `Cargo.lock`, the Cargo configuration and the
/// compiler. The driver's own sources are not part of it: what the driver
/// prepares is hashed as built fixture bytes. With `--no-build` there is no
/// such attestation, so the runner is the binary's hash.
pub fn runner_identity(
    root: &Path,
    built_from_sources: bool,
    binary: &Path,
) -> Result<Value, String> {
    if !built_from_sources {
        return Ok(json!({"binary_sha256": crate::hash(binary)?}));
    }
    let metadata = Command::new("cargo")
        .current_dir(root)
        .args(["metadata", "--offline", "--locked", "--format-version", "1"])
        .output()
        .map_err(|e| e.to_string())?;
    if !metadata.status.success() {
        return Err("cargo metadata failed while identifying the scenario runner".into());
    }
    let metadata: Value = serde_json::from_slice(&metadata.stdout).map_err(|e| e.to_string())?;
    let crates = workspace_closure(&metadata, "smoke-core")?;
    let mut files = Vec::new();
    for directory in &crates {
        let relative = directory.strip_prefix(root).unwrap_or(directory);
        let listed = Command::new("git")
            .current_dir(root)
            .args(["ls-files", "-co", "--exclude-standard", "--"])
            .arg(relative)
            .output()
            .map_err(|e| e.to_string())?;
        if !listed.status.success() {
            return Err("git ls-files failed while identifying the scenario runner".into());
        }
        for name in String::from_utf8_lossy(&listed.stdout).lines() {
            let path = root.join(name);
            if path.is_file() {
                files.push((name.to_owned(), path));
            }
        }
    }
    for name in ["Cargo.lock", ".cargo/config.toml", "rust-toolchain.toml"] {
        let path = root.join(name);
        if path.is_file() {
            files.push((name.to_owned(), path));
        }
    }
    files.sort();
    files.dedup();
    let rustc = Command::new("rustc")
        .current_dir(root)
        .arg("-vV")
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
        .unwrap_or_default();
    Ok(json!({
        "sources_sha256": digest_files(&files)?,
        "crates": crates.iter().map(|c| c.strip_prefix(root).unwrap_or(c).display().to_string()).collect::<Vec<_>>(),
        "rustc": rustc.lines().find(|l| l.starts_with("release:") || l.starts_with("commit-hash:")).map(str::to_owned),
        "rustc_sha256": format!("{:x}", Sha256::digest(rustc.as_bytes())),
    }))
}

/// Directories of `package` and the workspace (path) crates it depends on.
fn workspace_closure(metadata: &Value, package: &str) -> Result<BTreeSet<PathBuf>, String> {
    let packages: HashMap<&str, &Value> = metadata["packages"]
        .as_array()
        .ok_or("cargo metadata has no packages")?
        .iter()
        .filter_map(|p| Some((p["id"].as_str()?, p)))
        .collect();
    let nodes: HashMap<&str, Vec<&str>> = metadata["resolve"]["nodes"]
        .as_array()
        .ok_or("cargo metadata has no resolve graph")?
        .iter()
        .filter_map(|n| {
            Some((
                n["id"].as_str()?,
                n["dependencies"]
                    .as_array()?
                    .iter()
                    .filter_map(Value::as_str)
                    .collect(),
            ))
        })
        .collect();
    let start = packages
        .iter()
        .find(|(_, p)| p["name"] == package && p["source"].is_null())
        .map(|(id, _)| *id)
        .ok_or_else(|| format!("workspace has no {package} package"))?;
    let mut seen = BTreeSet::new();
    let mut stack = vec![start];
    let mut directories = BTreeSet::new();
    while let Some(id) = stack.pop() {
        if !seen.insert(id) {
            continue;
        }
        let Some(package) = packages.get(id).filter(|p| p["source"].is_null()) else {
            continue;
        };
        let manifest = PathBuf::from(package["manifest_path"].as_str().unwrap_or_default());
        directories.insert(manifest.parent().unwrap_or(Path::new("")).to_owned());
        stack.extend(nodes.get(id).into_iter().flatten());
    }
    Ok(directories)
}

/// Environment entries that only route logs or artifacts, not behavior.
const UNKEYED_ENVIRONMENT: &[&str] = &[
    "L6_MIDI_NAMESPACE",
    "L6_LOG_FORMAT",
    "RUST_LOG",
    "L6_PATCH_ARTIFACT_DIR",
];

/// An invocation with its run- and case-specific paths replaced, so the same
/// case in another run directory normalizes identically.
pub fn normalized_invocation(
    arguments: &[String],
    environment: &BTreeMap<String, String>,
    run: &Path,
    case: &Path,
) -> Value {
    let (run, case) = (run.display().to_string(), case.display().to_string());
    let normalize = |text: &str| text.replace(&case, "{case}").replace(&run, "{run}");
    json!({
        "arguments": arguments.iter().map(|a| normalize(a)).collect::<Vec<_>>(),
        "environment": environment
            .iter()
            .filter(|(key, _)| !UNKEYED_ENVIRONMENT.contains(&key.as_str()))
            .map(|(key, value)| (key.clone(), normalize(value)))
            .collect::<BTreeMap<_, _>>(),
    })
}

/// Results of an earlier run that a case with the same key can stand on.
pub struct Earlier {
    results: HashMap<String, Reused>,
}

impl Earlier {
    pub fn load(run: &Path) -> Result<Self, String> {
        let run = fs::canonicalize(run).map_err(|e| format!("{}: {e}", run.display()))?;
        let results = crate::select::read_results(&run.join("report.json"))?;
        let mut usable = HashMap::new();
        for result in results {
            let Some(key) = &result.reuse_key else {
                continue;
            };
            let reused = match (&result.outcome, &result.reused) {
                // Keep pointing at the run that executed the case.
                (Outcome::Reused, Some(reused)) => reused.clone(),
                (Outcome::Pass | Outcome::Xfail, _) => Reused {
                    run: run.clone(),
                    artifacts: result.artifacts.clone(),
                    outcome: result.outcome,
                },
                _ => continue,
            };
            if matches!(reused.outcome, Outcome::Pass | Outcome::Xfail) {
                usable.insert(key.clone(), reused);
            }
        }
        Ok(Self { results: usable })
    }
    pub fn find(&self, key: &str) -> Option<&Reused> {
        self.results.get(key)
    }
    pub fn len(&self) -> usize {
        self.results.len()
    }
}

/// Mark `result` as reused from `earlier`.
pub fn reuse(result: &mut CaseResult, earlier: &Reused) {
    result.outcome = Outcome::Reused;
    result.phase = crate::report::Phase::Reuse;
    result.detail = format!(
        "{} with identical inputs in {}",
        earlier.outcome,
        earlier.run.display()
    );
    result.reused = Some(earlier.clone());
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::report::Phase;

    #[test]
    fn invocations_in_different_run_directories_normalize_identically() {
        let invocation = |run: &str, namespace: &str| {
            let case = format!("{run}/boot-l6-headless");
            normalized_invocation(
                &[
                    "__scenario".into(),
                    format!("{run}/spec/scenarios/boot.toml"),
                    "--state-dir".into(),
                    format!("{case}/state"),
                ],
                &BTreeMap::from([
                    ("L6_MIDI_NAMESPACE".to_string(), namespace.to_string()),
                    ("SMOKE_CASE".to_string(), "boot".to_string()),
                ]),
                Path::new(run),
                Path::new(&case),
            )
        };
        let first = invocation("/tmp/l6smk-a", "p1");
        assert_eq!(first, invocation("/tmp/l6smk-b", "p2"));
        assert_eq!(first["arguments"][1], "{run}/spec/scenarios/boot.toml");
        assert_eq!(first["arguments"][3], "{case}/state");
        assert!(first["environment"].get("L6_MIDI_NAMESPACE").is_none());
    }

    #[test]
    fn any_identity_change_changes_the_key() {
        let identity = json!({"qemu_sha256": "a", "card_sha256": "b", "scenario": "c"});
        let key = digest(&identity);
        for field in ["qemu_sha256", "card_sha256", "scenario"] {
            let mut changed = identity.clone();
            changed[field] = json!("changed");
            assert_ne!(digest(&changed), key, "{field}");
        }
        assert_eq!(digest(&identity.clone()), key);
    }

    #[test]
    fn only_passes_and_expected_failures_are_reused_and_chains_keep_their_origin() {
        let dir = std::env::temp_dir().join(format!("l6test-reuse-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let entry = |key: &str, outcome: &str, extra: &str| {
            format!(
                r#"{{"case":"{key}","model":"l6","backend":"headless","outcome":"{outcome}","detail":"","artifacts":"/a/{key}","phase":"execute","reuse_key":"{key}"{extra}}}"#
            )
        };
        let results = [
            entry("pass", "PASS", ""),
            entry("xfail", "XFAIL", ""),
            entry("fail", "FAIL", ""),
            entry("blocked", "BLOCKED", ""),
            entry("flaky", "FLAKY", ""),
            entry(
                "chained",
                "REUSED",
                r#","reused":{"run":"/origin","artifacts":"/origin/chained","outcome":"PASS"}"#,
            ),
        ];
        fs::write(
            dir.join("report.json"),
            format!(r#"{{"schema":2,"results":[{}]}}"#, results.join(",")),
        )
        .unwrap();
        let earlier = Earlier::load(&dir).unwrap();
        assert_eq!(earlier.len(), 3);
        assert!(earlier.find("fail").is_none());
        assert!(earlier.find("blocked").is_none());
        assert!(earlier.find("flaky").is_none());
        assert_eq!(earlier.find("xfail").unwrap().outcome, Outcome::Xfail);
        assert_eq!(earlier.find("chained").unwrap().run, Path::new("/origin"));
        let mut result = CaseResult::new("pass", "l6", "headless", Path::new("/b/pass"));
        reuse(&mut result, earlier.find("pass").unwrap());
        assert_eq!(
            (result.outcome, result.phase),
            (Outcome::Reused, Phase::Reuse)
        );
        assert_eq!(result.effective(), Outcome::Pass);
        fs::remove_dir_all(dir).unwrap();
    }
}
