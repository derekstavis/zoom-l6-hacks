//! Case identities, and reuse of earlier results with identical inputs.
//!
//! A case's [`CaseIdentity`] hashes everything that decides its result: QEMU,
//! the code that runs and asserts the scenario, the firmware, every file the
//! driver built into the case directory (patched images, SD bundle and build
//! reports, card, state), the scenario files, the reference images assertions
//! read, audio input, the exact invocation and the case's behavioral catalog
//! fields. Fixtures are hashed as built bytes, not as the specs or driver code
//! that produced them, so a change to the driver alone keeps every key.
//!
//! `--reuse RUN` reports a case as REUSED, without running it, when RUN passed
//! (or expectedly failed) the same case with the same key.
use crate::{
    catalog::Case,
    hashing::{digest, digest_files, hash},
    report::{CaseResult, Outcome, Phase, Reused},
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet, HashMap},
    env, fs,
    path::{Path, PathBuf},
    process::Command,
};

/// Everything that decides a case's result. Every field is required, so a
/// renamed input cannot silently drop out of the key.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct CaseIdentity {
    pub case: String,
    pub model: String,
    pub backend: String,
    pub qemu_sha256: String,
    /// The code that runs the scenario; see [`runner_identity`].
    pub runner: Value,
    /// The package the case boots or installs from, if any.
    pub firmware_package_sha256: Option<String>,
    /// The unpacked firmware directory the case boots.
    pub firmware: String,
    /// Every file the driver wrote into the case directory before it ran.
    pub case_files: String,
    pub card_sha256: Option<String>,
    pub state: String,
    /// The scenario file and its included fragments.
    pub scenario: String,
    /// The reference images assertions compare against.
    pub references: String,
    pub audio_input: Vec<(u8, String)>,
    /// The invocation with run-specific paths replaced.
    pub invocation: Value,
    /// The case's catalog fields that change how it runs or is judged.
    pub behavior: String,
}

impl CaseIdentity {
    pub fn key(&self) -> String {
        digest(&json!(self))
    }
    /// Names of the fields that differ from `other`.
    pub fn differences(&self, other: &Value) -> Vec<String> {
        let ours = json!(self);
        let mut fields: Vec<String> = ours
            .as_object()
            .into_iter()
            .flatten()
            .filter(|(name, value)| other.get(name.as_str()) != Some(value))
            .map(|(name, _)| name.clone())
            .collect();
        fields.sort();
        fields
    }
}

/// The catalog fields of `case` that change how it runs or how its result is
/// judged. Suite membership, expected-failure reasons and the scenario's path
/// are left out (its content is hashed separately), so moving a case between
/// suites or files keeps its key.
pub fn behavior(case: &Case) -> String {
    digest(&json!({
        "id": case.id,
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
    }))
}

/// Digest of a scenario's files. Files under the spec are named relative to
/// it, and generated scenarios as `generated/<file>`, so the same scenario in
/// two runs has the same digest.
pub fn scenario_digest(
    spec: &Path,
    generated: &Path,
    sources: &[PathBuf],
) -> Result<String, String> {
    let spec = fs::canonicalize(spec).map_err(|e| e.to_string())?;
    let generated = fs::canonicalize(generated).unwrap_or_else(|_| generated.to_owned());
    let files = sources
        .iter()
        .map(|path| {
            let name = if let Ok(relative) = path.strip_prefix(&spec) {
                relative.to_string_lossy().into_owned()
            } else if let Ok(relative) = path.strip_prefix(&generated) {
                format!("generated/{}", relative.to_string_lossy())
            } else {
                path.to_string_lossy().into_owned()
            };
            (name, path.clone())
        })
        .collect::<Vec<_>>();
    digest_files(&files)
}

/// Files in the case directory the identity covers separately, or that do
/// not affect the case: its own records, logs, and the card and state.
fn unkeyed_case_file(name: &str) -> bool {
    matches!(
        name,
        "inputs.json" | "invocation.json" | "result.json" | "reused.json" | "card.img" | "state"
    ) || name.ends_with(".log")
}

/// Digest of every file the driver wrote into `dir`, by relative name and
/// content. The scenario runner reads some of them by name (see
/// `smoke_core::fixture_layout`), so they decide its result.
pub fn case_files(dir: &Path) -> Result<String, String> {
    fn visit(root: &Path, path: &Path, files: &mut Vec<(String, PathBuf)>) -> Result<(), String> {
        let mut entries = fs::read_dir(path)
            .map_err(|e| format!("{}: {e}", path.display()))?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| e.to_string())?;
        entries.sort_by_key(|entry| entry.file_name());
        for entry in entries {
            let path = entry.path();
            let name = path
                .strip_prefix(root)
                .unwrap()
                .to_string_lossy()
                .into_owned();
            if path.parent() == Some(root) && unkeyed_case_file(&name) {
                continue;
            }
            if entry.file_type().map_err(|e| e.to_string())?.is_dir() {
                visit(root, &path, files)?;
            } else {
                files.push((name, path));
            }
        }
        Ok(())
    }
    let mut files = Vec::new();
    visit(dir, dir, &mut files)?;
    digest_files(&files)
}

/// Identity of the code that runs a scenario headless.
///
/// The headless runner is the driver binary, which links `smoke-core` and the
/// crates it uses. When `built` is the driver's freshly built executable and
/// matches the running one, the runner is identified by the source and
/// generated files its dep-info lists (every compiled source, `include_*!`
/// file and build-script output in the repository), minus the driver's own
/// sources, plus the manifests, Cargo configuration, compiler, profile and
/// flags. Otherwise it is the binary's hash, which is sound but changes with
/// any driver change.
pub fn runner_identity(root: &Path, built: Option<&Path>, running: &Path) -> Result<Value, String> {
    let binary_sha256 = hash(running)?;
    let fallback = |reason: &str| json!({"binary_sha256": binary_sha256, "reason": reason});
    let Some(built) = built else {
        return Ok(fallback("not built from the working tree"));
    };
    if hash(built)? != binary_sha256 {
        return Ok(fallback("the running driver is not the freshly built one"));
    }
    let Ok(dep_info) = fs::read_to_string(built.with_extension("d")) else {
        return Ok(fallback("no dep-info beside the built driver"));
    };
    let driver = root.join("tools/l6test");
    let mut files: Vec<(String, PathBuf)> = dep_info_paths(&dep_info)
        .into_iter()
        .filter(|path| path.starts_with(root) && !path.starts_with(&driver))
        .map(|path| {
            let name = path
                .strip_prefix(root)
                .unwrap()
                .to_string_lossy()
                .into_owned();
            (name, path)
        })
        .collect();
    for name in [
        "Cargo.toml",
        "Cargo.lock",
        "tools/l6test/Cargo.toml",
        ".cargo/config.toml",
        "rust-toolchain.toml",
    ] {
        let path = root.join(name);
        if path.is_file() {
            files.push((name.to_owned(), path));
        }
    }
    files.sort();
    files.dedup();
    if files.iter().any(|(_, path)| !path.is_file()) {
        return Ok(fallback("a dep-info source no longer exists"));
    }
    let rustc = Command::new("rustc")
        .current_dir(root)
        .arg("-vV")
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
        .unwrap_or_default();
    let flags: BTreeMap<String, String> = env::vars()
        .filter(|(key, _)| {
            key == "RUSTFLAGS"
                || key == "CARGO_ENCODED_RUSTFLAGS"
                || key.starts_with("CARGO_PROFILE_")
        })
        .collect();
    Ok(json!({
        "sources_sha256": digest_files(&files)?,
        "source_files": files.len(),
        "rustc": rustc,
        "debug_assertions": cfg!(debug_assertions),
        "flags": flags,
    }))
}

/// The prerequisite paths of a Makefile-style dep-info file (`target: a b`),
/// with `\ ` escapes and line continuations resolved.
fn dep_info_paths(text: &str) -> BTreeSet<PathBuf> {
    let mut paths = BTreeSet::new();
    for line in text.replace("\\\n", " ").lines() {
        let Some((_, prerequisites)) = line.split_once(": ") else {
            continue;
        };
        let mut current = String::new();
        let mut chars = prerequisites.chars().peekable();
        while let Some(c) = chars.next() {
            match c {
                '\\' if chars.peek() == Some(&' ') => {
                    current.push(' ');
                    chars.next();
                }
                ' ' | '\t' => {
                    if !current.is_empty() {
                        paths.insert(PathBuf::from(std::mem::take(&mut current)));
                    }
                }
                c => current.push(c),
            }
        }
        if !current.is_empty() {
            paths.insert(PathBuf::from(current));
        }
    }
    paths
}

/// Environment entries that only route logs or artifacts, not behavior.
const UNKEYED_ENVIRONMENT: &[&str] = &[
    "L6_MIDI_NAMESPACE",
    "L6_LOG_FORMAT",
    "RUST_LOG",
    "L6_PATCH_ARTIFACT_DIR",
];

/// Replace debugger socket paths (`/tmp/l6g<case digest>-<purpose>.sock`,
/// derived from the case directory) with `{gdb}-<purpose>.sock`.
fn normalize_gdb_sockets(text: &str) -> String {
    let mut out = String::new();
    let mut rest = text;
    while let Some(start) = rest.find("/tmp/l6g") {
        out.push_str(&rest[..start]);
        let after = &rest[start + "/tmp/l6g".len()..];
        let digits = after.chars().take_while(char::is_ascii_hexdigit).count();
        if digits > 0 && after[digits..].starts_with('-') {
            out.push_str("{gdb}");
            rest = &after[digits..];
        } else {
            out.push_str("/tmp/l6g");
            rest = after;
        }
    }
    out.push_str(rest);
    out
}

/// Replace run-cache entry paths (`{run}/cache/<kind>/<entry>`) with
/// `{cache}/<kind>`. What a case reads from the cache is hashed by content
/// (for example the `firmware` digest), so the entry's name must not matter.
fn normalize_cache_paths(text: &str) -> String {
    const PREFIX: &str = "{run}/cache/";
    let mut out = String::new();
    let mut rest = text;
    while let Some(start) = rest.find(PREFIX) {
        out.push_str(&rest[..start]);
        let after = &rest[start + PREFIX.len()..];
        let kind_len = after.find('/').unwrap_or(after.len());
        let entry = &after[(kind_len + 1).min(after.len())..];
        let entry_len = entry
            .find(|c: char| !c.is_ascii_hexdigit())
            .unwrap_or(entry.len());
        if kind_len < after.len() && entry_len > 0 {
            out.push_str("{cache}/");
            out.push_str(&after[..kind_len]);
            rest = &entry[entry_len..];
        } else {
            out.push_str(PREFIX);
            rest = after;
        }
    }
    out.push_str(rest);
    out
}

/// An invocation with its run- and case-specific paths replaced, so the same
/// case in another run directory normalizes identically.
pub fn normalized_invocation(
    arguments: &[String],
    environment: &BTreeMap<String, String>,
    run: &Path,
    case: &Path,
) -> Value {
    let (run, case) = (run.display().to_string(), case.display().to_string());
    let normalize = |text: &str| {
        normalize_cache_paths(&normalize_gdb_sockets(
            &text.replace(&case, "{case}").replace(&run, "{run}"),
        ))
    };
    json!({
        "arguments": arguments.iter().map(|a| normalize(a)).collect::<Vec<_>>(),
        "environment": environment
            .iter()
            .filter(|(key, _)| !UNKEYED_ENVIRONMENT.contains(&key.as_str()))
            .map(|(key, value)| (key.clone(), normalize(value)))
            .collect::<BTreeMap<_, _>>(),
    })
}

/// The results of an earlier run, by key and by job.
pub struct Earlier {
    run: PathBuf,
    by_key: HashMap<String, Reused>,
    by_job: HashMap<(String, String, String), CaseResult>,
}

impl Earlier {
    pub fn load(run: &Path) -> Result<Self, String> {
        let run = fs::canonicalize(run).map_err(|e| format!("{}: {e}", run.display()))?;
        let results = crate::report::read_results(&run.join("report.json"))?;
        let mut by_key = HashMap::new();
        let mut by_job = HashMap::new();
        for result in results {
            if let Some(key) = &result.reuse_key
                && let Some(reused) = usable(&run, &result)
            {
                by_key.insert(key.clone(), reused);
            }
            by_job.insert(
                (
                    result.case.clone(),
                    result.model.clone(),
                    result.backend.clone(),
                ),
                result,
            );
        }
        Ok(Self {
            run,
            by_key,
            by_job,
        })
    }

    /// The earlier result `identity` can stand on.
    pub fn find(&self, identity: &CaseIdentity) -> Option<&Reused> {
        self.by_key.get(&identity.key())
    }

    /// Why the same job in the earlier run cannot be reused.
    pub fn explain_miss(&self, identity: &CaseIdentity) -> String {
        let job = (
            identity.case.clone(),
            identity.model.clone(),
            identity.backend.clone(),
        );
        let Some(earlier) = self.by_job.get(&job) else {
            return format!("{} did not run this case", self.run.display());
        };
        if usable(&self.run, earlier).is_none() {
            return format!(
                "{} was {} there, or its artifacts are gone",
                earlier.case, earlier.outcome
            );
        }
        match &earlier.identity {
            Some(other) => {
                let fields = identity.differences(other);
                if fields.is_empty() {
                    "identical inputs, but recorded under another key".into()
                } else {
                    format!("inputs differ: {}", fields.join(", "))
                }
            }
            None => "the earlier result has no identity".into(),
        }
    }

    pub fn len(&self) -> usize {
        self.by_key.len()
    }
}

/// The reuse record for an earlier result, if it can be reused: it passed or
/// expectedly failed (directly or by reuse), and the artifacts of the run that
/// executed it still exist.
fn usable(run: &Path, result: &CaseResult) -> Option<Reused> {
    let reused = match (&result.outcome, &result.reused) {
        // Keep pointing at the run that executed the case.
        (Outcome::Reused, Some(reused)) => reused.clone(),
        (Outcome::Pass | Outcome::Xfail, _) => Reused {
            run: run.to_owned(),
            artifacts: result.artifacts.clone(),
            outcome: result.outcome,
        },
        _ => return None,
    };
    (matches!(reused.outcome, Outcome::Pass | Outcome::Xfail) && reused.artifacts.is_dir())
        .then_some(reused)
}

/// Mark `result` as reused from `earlier`.
pub fn reuse(result: &mut CaseResult, earlier: &Reused) {
    result.outcome = Outcome::Reused;
    result.phase = Phase::Reuse;
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

    fn identity() -> CaseIdentity {
        CaseIdentity {
            case: "boot".into(),
            model: "l6".into(),
            backend: "headless".into(),
            qemu_sha256: "q".into(),
            runner: json!({"sources_sha256": "r"}),
            firmware_package_sha256: Some("p".into()),
            firmware: "f".into(),
            case_files: "c".into(),
            card_sha256: None,
            state: "s".into(),
            scenario: "sc".into(),
            references: "img".into(),
            audio_input: vec![],
            invocation: json!({"arguments": ["a"]}),
            behavior: "b".into(),
        }
    }

    #[test]
    fn invocations_in_different_run_directories_normalize_identically() {
        let invocation = |run: &str, namespace: &str, gdb: &str| {
            let case = format!("{run}/boot-l6-headless");
            normalized_invocation(
                &[
                    "__scenario".into(),
                    format!("{run}/spec/scenarios/boot.toml"),
                    "--state-dir".into(),
                    format!("{case}/state"),
                    "--gdb-socket".into(),
                    format!("/tmp/l6g{gdb}-main.sock"),
                ],
                &BTreeMap::from([
                    ("L6_MIDI_NAMESPACE".to_string(), namespace.to_string()),
                    ("SMOKE_CASE".to_string(), "boot".to_string()),
                ]),
                Path::new(run),
                Path::new(&case),
            )
        };
        let first = invocation("/tmp/l6smk-a", "p1", "0123456789abcdef");
        assert_eq!(first, invocation("/tmp/l6smk-b", "p2", "fedcba9876543210"));
        assert_eq!(first["arguments"][1], "{run}/spec/scenarios/boot.toml");
        assert_eq!(first["arguments"][3], "{case}/state");
        assert_eq!(first["arguments"][5], "{gdb}-main.sock");
        assert!(first["environment"].get("L6_MIDI_NAMESPACE").is_none());
        assert_eq!(normalize_gdb_sockets("/tmp/l6gx"), "/tmp/l6gx");
        assert_eq!(
            normalize_cache_paths("{run}/cache/unpacked/fd3746cf8e9d2272/x"),
            "{cache}/unpacked/x"
        );
        assert_eq!(
            normalize_cache_paths("--firmware-dir {run}/cache/unpacked/060a2d2a0a0bcc80"),
            "--firmware-dir {cache}/unpacked"
        );
        assert_eq!(normalize_cache_paths("{run}/cache/"), "{run}/cache/");
    }

    #[test]
    fn any_identity_change_changes_the_key_and_is_explained() {
        let base = identity();
        let mut changed = base.clone();
        changed.case_files = "other".into();
        assert_ne!(base.key(), changed.key());
        assert_eq!(base.differences(&json!(changed)), ["case_files"]);
        assert_eq!(base.key(), identity().key());
    }

    #[test]
    fn behavior_ignores_suites_and_expected_failure_reasons() {
        let case: Case = toml::from_str(
            "id = 'boot'\nsuites = ['quick']\nmodels = ['l6']\nscenario = 'scenarios/boot.toml'\n\
             [[expected_failure]]\nmodel = 'l6'\nstep = 3\ncontains = 'x'\nreason = 'first'\n",
        )
        .unwrap();
        let mut moved = case.clone();
        moved.suites = vec!["full".into()];
        moved.expected_failure[0].reason = "reworded".into();
        assert_eq!(behavior(&case), behavior(&moved));
        moved.wall_timeout_s += 1;
        assert_ne!(behavior(&case), behavior(&moved));
    }

    #[test]
    fn case_files_cover_driver_outputs_but_not_records_or_logs() {
        let dir = std::env::temp_dir().join(format!("l6test-case-files-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join("sd-patches-build")).unwrap();
        fs::write(dir.join("sd-patches-build/patch.json"), b"{}").unwrap();
        let before = case_files(&dir).unwrap();
        fs::write(dir.join("inputs.json"), b"paths").unwrap();
        fs::write(dir.join("stdout.log"), b"log").unwrap();
        assert_eq!(case_files(&dir).unwrap(), before);
        fs::write(dir.join("sd-patches-build/patch.json"), b"{\"changed\":1}").unwrap();
        assert_ne!(case_files(&dir).unwrap(), before);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn dep_info_paths_resolve_escapes_and_continuations() {
        let text = "/t/debug/l6test: /r/a.rs /r/with\\ space.rs \\\n /r/b.rs\n\n/r/a.rs:\n";
        let paths = dep_info_paths(text);
        assert_eq!(
            paths,
            BTreeSet::from([
                PathBuf::from("/r/a.rs"),
                PathBuf::from("/r/with space.rs"),
                PathBuf::from("/r/b.rs")
            ])
        );
    }

    #[test]
    fn only_usable_passes_are_reused_and_misses_are_explained() {
        let dir = std::env::temp_dir().join(format!("l6test-reuse-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        let artifacts = dir.join("artifacts");
        fs::create_dir_all(&artifacts).unwrap();
        let mut entries = Vec::new();
        for (case, outcome, artifacts) in [
            ("pass", Outcome::Pass, artifacts.clone()),
            ("xfail", Outcome::Xfail, artifacts.clone()),
            ("fail", Outcome::Fail, artifacts.clone()),
            ("gone", Outcome::Pass, dir.join("missing")),
        ] {
            let mut identity = identity();
            identity.case = case.into();
            let mut result = CaseResult::new(case, "l6", "headless", &artifacts);
            result.outcome = outcome;
            result.phase = Phase::Execute;
            result.reuse_key = Some(identity.key());
            result.identity = Some(json!(identity));
            entries.push(result);
        }
        fs::write(
            dir.join("report.json"),
            serde_json::to_vec(&json!({"schema": 2, "results": entries})).unwrap(),
        )
        .unwrap();
        let earlier = Earlier::load(&dir).unwrap();
        let mut id = identity();
        for (case, reusable) in [
            ("pass", true),
            ("xfail", true),
            ("fail", false),
            ("gone", false),
        ] {
            id.case = case.into();
            assert_eq!(earlier.find(&id).is_some(), reusable, "{case}");
        }
        id.case = "pass".into();
        id.qemu_sha256 = "new".into();
        assert!(earlier.find(&id).is_none());
        assert_eq!(earlier.explain_miss(&id), "inputs differ: qemu_sha256");
        id.case = "absent".into();
        assert!(earlier.explain_miss(&id).contains("did not run"));
        let mut result = CaseResult::new("pass", "l6", "headless", Path::new("/b"));
        id = identity();
        id.case = "pass".into();
        reuse(&mut result, earlier.find(&id).unwrap());
        assert_eq!(
            (result.outcome, result.phase),
            (Outcome::Reused, Phase::Reuse)
        );
        assert_eq!(result.effective(), Outcome::Pass);
        fs::remove_dir_all(dir).unwrap();
    }
}
