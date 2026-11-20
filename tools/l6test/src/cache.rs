//! Fixture outputs shared by the cases of one run.
//!
//! Many cases build the same patched firmware, SD bundle, unpacked package or
//! device state. Each distinct build runs once per run, under
//! `<run>/cache/<kind>/<digest>` (its key beside it in `<digest>.key`), and
//! cases copy the result into their own directory, so every case still owns
//! and records its inputs. A key names every input of a build except the
//! repository sources, which a run assumes do not change while it runs; the
//! cache never outlives its run.
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, HashMap},
    fs,
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex, OnceLock,
        atomic::{AtomicU64, Ordering},
    },
};

type Entry = Arc<OnceLock<Result<PathBuf, String>>>;

#[derive(Default)]
struct Stats {
    hits: u64,
    builds: u64,
}

pub struct RunCache {
    root: PathBuf,
    /// When false every request builds afresh, for comparing cached and
    /// uncached fixtures.
    enabled: bool,
    /// Whether copying a file into a case clones it (and keeps sparse card
    /// images sparse) on this volume.
    clones: bool,
    builds: AtomicU64,
    entries: Mutex<HashMap<String, Entry>>,
    stats: Mutex<BTreeMap<&'static str, Stats>>,
    file_hashes: Mutex<HashMap<PathBuf, String>>,
    values: Mutex<HashMap<String, String>>,
}

impl RunCache {
    pub fn new(root: PathBuf) -> Self {
        let clones = clones_files(&root);
        Self {
            root,
            enabled: true,
            clones,
            builds: AtomicU64::new(0),
            entries: Mutex::default(),
            stats: Mutex::default(),
            file_hashes: Mutex::default(),
            values: Mutex::default(),
        }
    }

    /// A cache that builds every request separately.
    pub fn disabled(root: PathBuf) -> Self {
        Self {
            enabled: false,
            ..Self::new(root)
        }
    }

    /// Whether a copy into a case directory is a clone: then a large sparse
    /// card image can be built once and copied for free.
    pub fn clones(&self) -> bool {
        self.clones
    }

    /// The directory holding `kind`'s output for `key`, building it with
    /// `build` the first time. `build` receives a path that does not exist yet
    /// and creates it. Concurrent callers with the same key wait for one build.
    /// A failed build is remembered, so every case that needs it reports the
    /// same error without rebuilding; builds are deterministic, but a
    /// transient error such as a full disk also fails every such case.
    pub fn directory(
        &self,
        kind: &'static str,
        key: &str,
        build: impl FnOnce(&Path) -> Result<(), String>,
    ) -> Result<PathBuf, String> {
        let mut digest = format!("{:x}", Sha256::digest(format!("{kind}\0{key}").as_bytes()));
        if !self.enabled {
            let n = self.builds.fetch_add(1, Ordering::Relaxed);
            digest = format!("{:x}", Sha256::digest(format!("{digest}\0{n}").as_bytes()));
        }
        let entry = self
            .entries
            .lock()
            .unwrap()
            .entry(digest.clone())
            .or_default()
            .clone();
        let mut built = false;
        let result = entry.get_or_init(|| {
            built = true;
            let directory = self.root.join(kind);
            let name = &digest[..16];
            let target = directory.join(name);
            let staging = directory.join(format!("{name}.building"));
            let _ = fs::remove_dir_all(&staging);
            fs::create_dir_all(&directory).map_err(|e| e.to_string())?;
            fs::write(directory.join(format!("{name}.key")), key).map_err(|e| e.to_string())?;
            // Builders such as `l6patch` and `l6fw::unpack` create their
            // output directory, so `build` receives a path that does not exist.
            build(&staging)?;
            fs::create_dir_all(&staging).map_err(|e| e.to_string())?;
            fs::rename(&staging, &target).map_err(|e| e.to_string())?;
            Ok(target)
        });
        let mut stats = self.stats.lock().unwrap();
        let stats = stats.entry(kind).or_default();
        if built {
            stats.builds += 1;
        } else {
            stats.hits += 1;
        }
        result.clone()
    }

    /// A small value computed once per run for `key`.
    pub fn value(
        &self,
        key: &str,
        compute: impl FnOnce() -> Result<String, String>,
    ) -> Result<String, String> {
        if let Some(value) = self.values.lock().unwrap().get(key) {
            return Ok(value.clone());
        }
        let value = compute()?;
        self.values
            .lock()
            .unwrap()
            .insert(key.to_owned(), value.clone());
        Ok(value)
    }

    /// SHA-256 of a file that does not change during the run (a configured
    /// package or a cached output), computed once.
    pub fn file_hash(&self, path: &Path) -> Result<String, String> {
        if let Some(hash) = self.file_hashes.lock().unwrap().get(path) {
            return Ok(hash.clone());
        }
        let hash = crate::hashing::hash(path)?;
        self.file_hashes
            .lock()
            .unwrap()
            .insert(path.to_owned(), hash.clone());
        Ok(hash)
    }

    /// Hits and builds per kind, for the report.
    pub fn stats(&self) -> Value {
        let stats = self.stats.lock().unwrap();
        json!(
            stats
                .iter()
                .map(|(kind, s)| (
                    kind.to_string(),
                    json!({"hits": s.hits, "builds": s.builds})
                ))
                .collect::<BTreeMap<_, _>>()
        )
    }
}

/// Copy a tree. On a volume that clones (APFS), `fs::copy` clones each file,
/// so a copy costs no data.
pub fn copy_tree(source: &Path, target: &Path) -> Result<(), String> {
    fs::create_dir_all(target).map_err(|e| e.to_string())?;
    for entry in fs::read_dir(source).map_err(|e| e.to_string())? {
        let entry = entry.map_err(|e| e.to_string())?;
        let path = entry.path();
        let destination = target.join(entry.file_name());
        let kind = entry.file_type().map_err(|e| e.to_string())?;
        if kind.is_symlink() {
            return Err(format!("tree contains a symlink: {}", path.display()));
        } else if kind.is_dir() {
            copy_tree(&path, &destination)?;
        } else {
            fs::copy(&path, &destination).map_err(|e| format!("{}: {e}", destination.display()))?;
        }
    }
    Ok(())
}

/// Whether files under `root` can be cloned, tried once with a small file.
#[cfg(target_os = "macos")]
fn clones_files(root: &Path) -> bool {
    use std::{ffi::CString, os::unix::ffi::OsStrExt};
    if fs::create_dir_all(root).is_err() {
        return false;
    }
    let source = root.join(".clone-probe");
    let target = root.join(".clone-probe.copy");
    let _ = fs::remove_file(&target);
    if fs::write(&source, b"probe").is_err() {
        return false;
    }
    let path = |p: &Path| CString::new(p.as_os_str().as_bytes()).ok();
    let cloned = match (path(&source), path(&target)) {
        (Some(s), Some(t)) => unsafe { libc::clonefile(s.as_ptr(), t.as_ptr(), 0) == 0 },
        _ => false,
    };
    let _ = fs::remove_file(&source);
    let _ = fs::remove_file(&target);
    cloned
}

#[cfg(not(target_os = "macos"))]
fn clones_files(_root: &Path) -> bool {
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;

    fn temp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "l6test-cache-{name}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = fs::remove_dir_all(&dir);
        dir
    }

    #[test]
    fn each_key_builds_once_even_when_requested_concurrently() {
        let dir = temp("once");
        let cache = RunCache::new(dir.clone());
        let builds = AtomicUsize::new(0);
        std::thread::scope(|scope| {
            for _ in 0..8 {
                scope.spawn(|| {
                    let path = cache
                        .directory("thing", "a", |out| {
                            builds.fetch_add(1, Ordering::SeqCst);
                            std::thread::sleep(std::time::Duration::from_millis(20));
                            fs::create_dir_all(out).map_err(|e| e.to_string())?;
                            fs::write(out.join("file"), b"built").map_err(|e| e.to_string())
                        })
                        .unwrap();
                    assert_eq!(fs::read(path.join("file")).unwrap(), b"built");
                });
            }
        });
        assert_eq!(builds.load(Ordering::SeqCst), 1);
        let other = cache
            .directory("thing", "b", |out| {
                fs::create_dir_all(out).map_err(|e| e.to_string())?;
                fs::write(out.join("file"), b"other").map_err(|e| e.to_string())
            })
            .unwrap();
        assert_eq!(fs::read(other.join("file")).unwrap(), b"other");
        assert_eq!(cache.stats()["thing"], json!({"hits": 7, "builds": 2}));
        // The key is recorded beside the entry, not inside it.
        assert!(!other.join(".key").exists());
        let name = other.file_name().unwrap().to_string_lossy().into_owned();
        assert_eq!(
            fs::read_to_string(other.with_file_name(format!("{name}.key"))).unwrap(),
            "b"
        );
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_disabled_cache_builds_every_request() {
        let dir = temp("disabled");
        let cache = RunCache::disabled(dir.clone());
        let mut builds = 0;
        for _ in 0..3 {
            cache
                .directory("thing", "a", |out| {
                    builds += 1;
                    fs::create_dir_all(out).map_err(|e| e.to_string())
                })
                .unwrap();
        }
        assert_eq!(builds, 3);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_failed_build_is_reported_to_every_case_without_rebuilding() {
        let dir = temp("fail");
        let cache = RunCache::new(dir.clone());
        let mut builds = 0;
        for _ in 0..3 {
            let error = cache
                .directory("thing", "broken", |_| {
                    builds += 1;
                    Err("l6patch failed".into())
                })
                .unwrap_err();
            assert_eq!(error, "l6patch failed");
        }
        assert_eq!(builds, 1);
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn copies_are_independent_of_the_cached_tree() {
        let dir = temp("copy");
        let cache = RunCache::new(dir.join("cache"));
        let cached = cache
            .directory("tree", "k", |out| {
                fs::create_dir_all(out.join("sub")).map_err(|e| e.to_string())?;
                fs::write(out.join("sub/flash"), b"original").map_err(|e| e.to_string())
            })
            .unwrap();
        let case = dir.join("case");
        copy_tree(&cached, &case).unwrap();
        fs::write(case.join("sub/flash"), b"changed").unwrap();
        assert_eq!(fs::read(cached.join("sub/flash")).unwrap(), b"original");
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    #[cfg(target_os = "macos")]
    fn the_temporary_directory_clones_on_apfs() {
        // macOS keeps its temporary directory on the APFS system volume.
        let dir = temp("probe");
        assert!(clones_files(&dir));
        let _ = fs::remove_dir_all(dir);
    }
}
