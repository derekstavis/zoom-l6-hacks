//! Reproducible suite driver. All guest runners are supervised child processes.
mod cache;
mod case;
mod catalog;
mod config;
mod fixture;
mod gui_capture;
mod gui_probe;
mod hashing;
mod identity;
mod patch_matrix;
mod report;
mod schedule;
mod screen_gif;
mod select;
mod supervise;

use cache::RunCache;
use case::{Context, run_case};
use catalog::Case;
use config::{Config, load_config};
use fixture::case_fixture;
use hashing::{files, hash};
use l6max_host::engine::Options;
use report::{CaseResult, Outcome};
use schedule::schedule;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use smoke_core::Scenario;
use std::{
    collections::{BTreeMap, BTreeSet},
    env, fs,
    path::{Path, PathBuf},
    process::Command,
    thread,
    time::{SystemTime, UNIX_EPOCH},
};
use supervise::{interrupt, supervise};

struct Args {
    command: String,
    config: PathBuf,
    suite: String,
    case: Option<String>,
    backend: String,
    model: String,
    output: Option<PathBuf>,
    build: bool,
    jobs: usize,
    qemu: Option<PathBuf>,
    app: Option<PathBuf>,
    /// `list --format json`.
    json: bool,
    retries: usize,
    strict: bool,
    shard: Option<select::Shard>,
    rerun: Option<PathBuf>,
    only: Vec<Outcome>,
    /// Build every fixture and compute identities without booting.
    fixtures_only: bool,
    /// Build every fixture per case, without the run cache.
    no_cache: bool,
    /// Replace display references that are missing or differ with the
    /// rendered display, for review in the diff.
    update_references: bool,
}
/// Each guest runs one single-threaded TCG engine plus its scenario process.
/// Leave headroom so concurrent guests keep up with real time.
fn default_jobs() -> usize {
    thread::available_parallelism().map_or(1, |cores| (cores.get() / 4).clamp(1, 4))
}
fn parse_args() -> Result<Args, String> {
    let mut args = env::args().skip(1);
    let command = args.next().unwrap_or_else(|| "help".into());
    let mut result = Args {
        command,
        config: PathBuf::from(l6max_host::config::FILE_NAME),
        suite: "quick".into(),
        case: None,
        backend: "headless".into(),
        model: "all".into(),
        output: None,
        build: true,
        jobs: default_jobs(),
        qemu: None,
        app: None,
        json: false,
        retries: 0,
        strict: false,
        shard: None,
        rerun: None,
        only: select::RERUN_DEFAULT.to_vec(),
        fixtures_only: false,
        no_cache: false,
        update_references: false,
    };
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--config" => {
                result.config = PathBuf::from(args.next().ok_or("--config needs a file")?)
            }
            "--suite" => result.suite = args.next().ok_or("--suite needs a name")?,
            "--case" => result.case = Some(args.next().ok_or("--case needs a name")?),
            "--backend" => {
                result.backend = args.next().ok_or("--backend needs headless, gui or both")?
            }
            "--model" => result.model = args.next().ok_or("--model needs l6, l6max or all")?,
            "--output" => {
                result.output = Some(PathBuf::from(
                    args.next().ok_or("--output needs a directory")?,
                ))
            }
            "--no-build" => result.build = false,
            "--qemu" => {
                result.qemu = Some(PathBuf::from(args.next().ok_or("--qemu needs a binary")?))
            }
            "--app" => {
                result.app = Some(PathBuf::from(
                    args.next().ok_or("--app needs a packaged application")?,
                ))
            }
            "--format" => match args.next().as_deref() {
                Some("json") => result.json = true,
                Some("text") => result.json = false,
                _ => return Err("--format needs text or json".into()),
            },
            "--retries" => {
                result.retries = args
                    .next()
                    .and_then(|value| value.parse().ok())
                    .ok_or("--retries needs a number")?
            }
            "--strict" => result.strict = true,
            "--fixtures-only" => result.fixtures_only = true,
            "--no-cache" => result.no_cache = true,
            "--update-references" => result.update_references = true,
            "--shard" => {
                result.shard = Some(select::Shard::parse(
                    &args.next().ok_or("--shard needs I/N")?,
                )?)
            }
            "--rerun" => {
                result.rerun = Some(PathBuf::from(
                    args.next()
                        .ok_or("--rerun needs a report or run directory")?,
                ))
            }
            "--only" => {
                result.only = select::parse_outcomes(&args.next().ok_or("--only needs outcomes")?)?
            }
            "--jobs" => {
                result.jobs = args
                    .next()
                    .and_then(|value| value.parse().ok())
                    .filter(|jobs| *jobs > 0)
                    .ok_or("--jobs needs a positive number")?
            }
            _ => return Err(format!("unknown argument {arg}")),
        }
    }
    if !["headless", "gui", "both"].contains(&result.backend.as_str())
        || !["l6", "l6max", "all"].contains(&result.model.as_str())
    {
        return Err("invalid backend or model".into());
    }
    if result.rerun.is_some() && result.case.is_some() {
        return Err("--rerun selects cases itself; do not combine it with --case".into());
    }
    Ok(result)
}
fn git(root: &Path, args: &[&str]) -> String {
    Command::new("git")
        .current_dir(root)
        .args(args)
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().into())
        .unwrap_or_default()
}

fn child_scenario() -> Result<(), String> {
    let mut args = env::args_os().skip(2);
    let path = PathBuf::from(args.next().ok_or("missing scenario")?);
    let options = Options::parse_args(args)?;
    smoke_core::run(Scenario::load_for_case(&path)?, options)
}
struct RunJournal(PathBuf);
impl Drop for RunJournal {
    fn drop(&mut self) {
        let path = self.0.join("report.json");
        let mut report = fs::read(&path)
            .ok()
            .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok())
            .unwrap_or_else(|| json!({"schema":report::SCHEMA}));
        if report.get("outcome").is_none() || report["outcome"] == "RUNNING" {
            report["outcome"] = json!("BLOCKED");
            report["error"] = json!(
                "driver exited before suite completion; inspect invocation/build logs and the terminal error"
            );
            let _ = fs::write(path, serde_json::to_vec_pretty(&report).unwrap());
        }
    }
}
fn execute() -> Result<i32, String> {
    let args = parse_args()?;
    let root = l6max_host::paths::repository_root();
    let mut catalog = catalog::load(root)?;
    if args.command == "list" {
        if args.json {
            let rows = catalog::matrix(&catalog)?;
            println!("{}", serde_json::to_string_pretty(&rows).unwrap());
            return Ok(0);
        }
        for case in &catalog.cases {
            println!(
                "{:<24} {:<12} {}{}",
                case.id,
                case.models.join(","),
                case.suites.join(","),
                if case.scenario.is_none() {
                    " [packaged app required]"
                } else {
                    ""
                }
            );
        }
        return Ok(0);
    }
    if !["run", "check"].contains(&args.command.as_str()) {
        println!(
            "Usage: cargo l6test list [--format text|json]\n       cargo l6test check|run [--config config.toml] [--suite NAME | --case ID,GLOB,... | --rerun RUN [--only OUTCOMES]] [--model l6|l6max|all] [--backend headless|gui|both] [--shard I/N] [--retries N] [--strict] [--fixtures-only] [--no-cache] [--update-references] [--output NEW_DIR] [--no-build] [--jobs N] [--qemu BINARY] [--app PACKAGED_APP]\n       cargo l6test probe gui --keys TARGET,... --volatile [engine options]\n       cargo l6test capture screen --out FILE.gif [capture options] --volatile [engine options]"
        );
        return Ok(0);
    }
    let rerun = args
        .rerun
        .as_ref()
        .map(|report| select::Rerun::load(report, &args.only))
        .transpose()?;
    if rerun.as_ref().is_some_and(select::Rerun::is_empty) {
        return Err("the report has no results with the selected outcomes".into());
    }
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let output = args
        .output
        .clone()
        .unwrap_or_else(|| PathBuf::from(format!("/tmp/l6smk-{stamp:x}")));
    if output.exists() {
        return Err(
            "output directory already exists; choose a new one to preserve evidence".into(),
        );
    }
    fs::create_dir_all(&output).map_err(|e| e.to_string())?;
    let output = fs::canonicalize(output).map_err(|e| e.to_string())?;
    tracing::info!(event="run_started", artifacts=%output.display(), suite=%args.suite, backend=%args.backend, model=%args.model,"smoke run started");
    let _journal = RunJournal(output.clone());
    // Cases run from the spec in place; only the generated scenarios are
    // written, into the run, so case processes can load them.
    let generated_dir = output.join("generated");
    catalog::write_generated(&mut catalog, &generated_dir)?;
    let blocked_report = |error: &str| {
        fs::write(
            output.join("report.json"),
            serde_json::to_vec_pretty(
                &json!({"schema":report::SCHEMA,"outcome":"BLOCKED","error":error}),
            )
            .unwrap(),
        )
        .map_err(|e| e.to_string())
    };
    let preflight = || -> Result<(Config, Value), String> {
        let config = load_config(
            &args.config,
            root,
            args.build,
            args.qemu.as_deref(),
            args.app.as_deref(),
        )?;
        let _shared = l6max_host::shared_memory::SharedRam::new(2048).map_err(|e| {
            format!("shared memory unavailable: {e}; rerun with the required sandbox permissions")
        })?;
        let mut untracked = BTreeMap::new();
        for name in git(root, &["ls-files", "--others", "--exclude-standard"]).lines() {
            let path = root.join(name);
            if path.is_file() {
                untracked.insert(name.to_owned(), hash(&path)?);
            }
        }
        let manifest = json!({"schema":report::SCHEMA,"tooling_commit":git(root,&["rev-parse","HEAD"]),"working_tree":git(root,&["status","--short"]),"tracked_diff_sha256":format!("{:x}",Sha256::digest(git(root,&["diff","--binary","HEAD"]).as_bytes())),"untracked":untracked,"qemu":config.qemu,"qemu_sha256":config.qemu_sha256,"qemu_provenance":"explicitly pinned external binary; build source not attested","scenario_files":catalog::spec_dirs(root)?.iter().map(|dir|Ok((dir.strip_prefix(root).unwrap().display().to_string(),files(dir)?))).collect::<Result<BTreeMap<_,_>,String>>()?,"generated_scenarios":files(&generated_dir)?,"board_sources":files(&root.join("emulator/qemu/hw/arm"))?,"qemu_patches":files(&root.join("emulator/qemu/patches"))?,"logging":{"format":env::var("L6_LOG_FORMAT").unwrap_or_else(|_|"json".into()),"filter":env::var("RUST_LOG").unwrap_or_else(|_|l6max_host::logging::DEFAULT_FILTER.into())},"driver_sha256":hash(&env::current_exe().map_err(|e|e.to_string())?)?,"suite":args.suite,"case":args.case,"backend":args.backend,"model":args.model,"shard":args.shard.map(|s|format!("{}/{}",s.index,s.count)),"rerun":args.rerun,"retries":args.retries});
        Ok((config, manifest))
    };
    let (config, manifest) = match preflight() {
        Ok(v) => v,
        Err(error) => {
            blocked_report(&error)?;
            return Err(error);
        }
    };
    fs::write(
        output.join("manifest.json"),
        serde_json::to_vec_pretty(&manifest).unwrap(),
    )
    .map_err(|e| e.to_string())?;
    let suite = catalog.suites.members(&args.suite)?;
    let cases: Vec<&Case> = catalog
        .cases
        .iter()
        .filter(|c| match (&rerun, &args.case) {
            (Some(rerun), _) => rerun.has_case(&c.id),
            (None, Some(selection)) => select::case_matches(selection, &c.id),
            (None, None) => c.suites.iter().any(|s| suite.contains(s)),
        })
        .collect();
    if let (None, Some(selection)) = (&rerun, &args.case) {
        let ids: Vec<&str> = catalog.cases.iter().map(|c| c.id.as_str()).collect();
        let unmatched = select::unmatched(selection, &ids);
        if !unmatched.is_empty() {
            return Err(format!("--case matches no case: {}", unmatched.join(", ")));
        }
    }
    if cases.is_empty() {
        return Err("no cases selected".into());
    }
    if args.command == "check" {
        fs::write(
            output.join("report.json"),
            serde_json::to_vec_pretty(
                &json!({"schema":report::SCHEMA,"outcome":"PASS","preflight_only":true}),
            )
            .unwrap(),
        )
        .map_err(|e| e.to_string())?;
        println!("Preflight passed. QEMU is pinned by binary hash for this run.");
        return Ok(0);
    }
    let backends: Vec<&str> = if args.backend == "both" {
        vec!["headless", "gui"]
    } else {
        vec![&args.backend]
    };
    // Expand cases into jobs before building, so only the executables the
    // selected jobs need are built.
    let mut jobs = vec![];
    for case in &cases {
        let scenario = case
            .scenario
            .as_ref()
            .map(|p| Scenario::load(&catalog::scenario_file(p)))
            .transpose()?;
        for model in case
            .models
            .iter()
            .filter(|m| args.model == "all" || *m == &args.model)
        {
            for backend in catalog::backends(case, scenario.as_ref(), &backends) {
                if args
                    .shard
                    .is_some_and(|s| !s.contains(&case.id, model, backend))
                    || rerun
                        .as_ref()
                        .is_some_and(|r| !r.contains(&case.id, model, backend))
                {
                    continue;
                }
                // GUI windows are the remaining host-wide exclusive resource.
                let exclusive = backend == "gui";
                let midi_namespace = format!("p{}-{}", std::process::id(), jobs.len());
                jobs.push(Job {
                    case,
                    model: model.clone(),
                    backend,
                    exclusive,
                    midi_namespace,
                    dir: output.join(format!("{}-{model}-{backend}", case.id.replace('/', "-"))),
                });
            }
        }
    }
    if jobs.is_empty() {
        return Err("no model/case combinations selected".into());
    }
    let mut packages = BTreeSet::new();
    let mut needs_l6patch = false;
    for job in &jobs {
        let case = job.case;
        if case.blocked.is_some() || case.scenario.is_none() && config.app.is_none() {
            continue;
        }
        if job.backend == "gui" && case.scenario.is_some() {
            packages.insert("l6max-gui");
        }
        needs_l6patch |= case
            .scenario
            .as_ref()
            .and_then(|path| Scenario::load(&catalog::scenario_file(path)).ok())
            .is_some_and(|scenario| scenario.needs_patch_builder());
    }
    if args.build {
        let mut command = Command::new("cargo");
        command
            .current_dir(root)
            .args(["build", "--offline", "--locked"]);
        for package in &packages {
            command.arg("-p").arg(package);
        }
        if needs_l6patch {
            command.args(["-p", "l6patch"]);
        }
        let built = supervise(command, &output, 600);
        if !matches!(built, Ok(true)) {
            let error = built
                .err()
                .unwrap_or_else(|| "build failed; inspect stdout.log/stderr.log".into());
            blocked_report(&error)?;
            return Err(error);
        }
    }
    // Resolve Cargo's actual output directory; CARGO_TARGET_DIR or workspace
    // configuration must not make us copy an older binary from root/target.
    let metadata = Command::new("cargo")
        .current_dir(root)
        .args([
            "metadata",
            "--offline",
            "--locked",
            "--no-deps",
            "--format-version",
            "1",
        ])
        .output()
        .map_err(|e| e.to_string())?;
    if !metadata.status.success() {
        return Err("cargo metadata failed while resolving built executables".into());
    }
    let metadata: Value = serde_json::from_slice(&metadata.stdout).map_err(|e| e.to_string())?;
    let built_dir = PathBuf::from(
        metadata["target_directory"]
            .as_str()
            .ok_or("Cargo metadata omitted target_directory")?,
    )
    .join("debug");
    fs::create_dir_all(output.join("bin")).map_err(|e| e.to_string())?;
    fs::copy(
        env::current_exe().map_err(|e| e.to_string())?,
        output.join("bin/l6test"),
    )
    .map_err(|e| e.to_string())?;
    for package in packages {
        fs::copy(built_dir.join(package), output.join("bin").join(package))
            .map_err(|e| e.to_string())?;
    }
    let cache = if args.no_cache {
        RunCache::disabled(output.join("cache"))
    } else {
        RunCache::new(output.join("cache"))
    };
    let ctx = Context {
        root,
        output: &output,
        config: &config,
        cache: &cache,
        patch_artifact_dir: &built_dir,
        fixtures_only: args.fixtures_only,
        update_references: args.update_references,
    };
    let run_job = |job: &Job| -> CaseResult {
        let case_span = tracing::info_span!("smoke_case",case=%job.case.id,model=%job.model,backend=job.backend,artifacts=%job.dir.display());
        let _entered = case_span.enter();
        tracing::info!(event = "case_started", "case started");
        let fixture = job
            .case
            .scenario
            .as_ref()
            .map(|path| Scenario::load(&catalog::scenario_file(path)))
            .transpose()
            .and_then(|scenario| {
                case_fixture(&config, &job.model, &job.case.id, scenario.as_ref())
            });
        let result = match fixture {
            Ok(fixture) => report::with_retries(args.retries, |attempt| {
                let (dir, namespace) = if attempt == 1 {
                    (job.dir.clone(), job.midi_namespace.clone())
                } else {
                    (
                        PathBuf::from(format!("{}.attempt-{attempt}", job.dir.display())),
                        format!("{}-a{attempt}", job.midi_namespace),
                    )
                };
                run_case(&ctx, &fixture, job.case, job.backend, &dir, &namespace)
            }),
            Err(detail) => CaseResult {
                detail,
                ..CaseResult::new(&job.case.id, &job.model, job.backend, &job.dir)
            },
        };
        let mut result = result;
        if let Ok(list) = fs::read_to_string(
            result
                .artifacts
                .join(smoke_core::devices::UPDATED_REFERENCES_FILE),
        ) {
            result.updated_references = list.lines().map(str::to_owned).collect();
        }
        match result.outcome {
            Outcome::Pass => {
                tracing::info!(event="case_finished",outcome=%result.outcome,detail=%result.detail,seconds=result.duration_s,"case completed")
            }
            Outcome::Blocked | Outcome::Xfail | Outcome::Flaky => {
                tracing::warn!(event="case_finished",outcome=%result.outcome,detail=%result.detail,seconds=result.duration_s,"case incomplete")
            }
            _ => {
                tracing::error!(event="case_finished",outcome=%result.outcome,detail=%result.detail,seconds=result.duration_s,"case failed")
            }
        }
        result
    };
    // Queue shareable cases first so an exclusive one never idles the pool.
    jobs.sort_by_key(|job| job.exclusive);
    let results = schedule(
        &jobs,
        args.jobs,
        |job| job.exclusive,
        run_job,
        |finished| {
            let finished: Vec<CaseResult> = finished.iter().map(|r| (*r).clone()).collect();
            fs::write(
                output.join("report.json"),
                serde_json::to_vec_pretty(&report::report("RUNNING", &finished, json!({})))
                    .unwrap(),
            )
            .map_err(|e| e.to_string())
        },
    )?;
    let (outcome, code) = report::summarize(&results, args.strict);
    let cache_stats = cache.stats();
    let report = report::report(
        &outcome.to_string(),
        &results,
        json!({"cache": cache_stats, "strict": args.strict, "fixtures_only": args.fixtures_only}),
    );
    fs::write(
        output.join("report.json"),
        serde_json::to_vec_pretty(&report).unwrap(),
    )
    .map_err(|e| e.to_string())?;
    report::write_summary(
        &output.join("summary.md"),
        outcome,
        &results,
        &cache_stats,
        args.fixtures_only,
    )?;
    Ok(code)
}
struct Job<'a> {
    case: &'a Case,
    model: String,
    backend: &'a str,
    exclusive: bool,
    midi_namespace: String,
    dir: PathBuf,
}
fn main() {
    let cli_args = env::args_os().skip(1).collect::<Vec<_>>();
    if let Err(error) = l6max_host::logging::init(l6max_host::logging::Format::Json) {
        eprintln!("{error}");
        std::process::exit(2);
    }
    let context = l6max_host::logging::context("driver");
    let _entered = context.enter();
    if cli_args.first().is_some_and(|arg| arg == "__scenario") {
        if let Err(error) = child_scenario() {
            tracing::error!(event="scenario_failed",%error,"scenario failed");
            std::process::exit(1);
        }
        return;
    }
    if cli_args.len() >= 2 && cli_args[0] == "probe" && cli_args[1] == "gui" {
        if let Err(error) = gui_probe::run(cli_args[2..].to_vec()) {
            tracing::error!(event="gui_probe_failed",%error,"GUI screen probe failed");
            std::process::exit(1);
        }
        return;
    }
    if cli_args.len() >= 2 && cli_args[0] == "capture" && cli_args[1] == "screen" {
        if let Err(error) = screen_gif::run(cli_args[2..].to_vec()) {
            tracing::error!(event="screen_capture_failed",%error,"L6max screen GIF capture failed");
            std::process::exit(1);
        }
        return;
    }
    if cli_args.len() >= 2 && cli_args[0] == "capture" && cli_args[1] == "gui" {
        if let Err(error) = gui_capture::run(cli_args[2..].to_vec()) {
            tracing::error!(event="gui_capture_failed",%error,"GPUI GUI capture failed");
            std::process::exit(1);
        }
        return;
    }
    unsafe {
        libc::signal(libc::SIGINT, interrupt as *const () as libc::sighandler_t);
        libc::signal(libc::SIGTERM, interrupt as *const () as libc::sighandler_t);
    }
    match execute() {
        Ok(code) => std::process::exit(code),
        Err(error) => {
            tracing::error!(event="run_blocked",%error,"smoke run blocked");
            std::process::exit(2);
        }
    }
}
