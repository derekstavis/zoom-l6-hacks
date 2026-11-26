//! Fixture outputs shared by the cases of one run.
//!
//! Many cases build the same patched firmware, SD bundle, unpacked package or
//! device state. Each distinct build runs once per run, under
//! `<run>/cache/<kind>/<digest>` (its key beside it in `<digest>.key`), and
//! cases copy the result into their own directory, so every case still owns
//! and records its inputs. A key names every input of a build except the
//! repository sources, which a run assumes do not change while it runs.
//! The opt-in persistent experiment additionally fingerprints the checkout
//! and isolates it from other checkouts.
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
    persistent: bool,
    // A run may keep using cached firmware after fixture preparation.
    _checkout_lease: Option<CacheLock>,
    native_toolchain: OnceLock<Result<patch_compile::PayloadToolchain, String>>,
    /// When false every request builds afresh, for comparing cached and
    /// uncached fixtures.
    enabled: bool,
    /// Whether copying a file into a case clones it (and keeps sparse card
    /// images sparse) on this volume.
    clones: bool,
    builds: AtomicU64,
    entries: Mutex<HashMap<String, Entry>>,
    stats: Mutex<BTreeMap<String, Stats>>,
    file_hashes: Mutex<HashMap<PathBuf, String>>,
    values: Mutex<HashMap<String, String>>,
}

#[derive(Clone, serde::Serialize, serde::Deserialize)]
pub(crate) struct SharedCacheConfig {
    root: PathBuf,
    persistent: bool,
    enabled: bool,
}

impl RunCache {
    pub(crate) fn shared_config(&self) -> SharedCacheConfig {
        SharedCacheConfig {
            root: self.root.clone(),
            persistent: self.persistent,
            enabled: self.enabled,
        }
    }

    /// The driver holds the checkout lease for this exact namespace while its
    /// supervised members use the same lock-protected cache entries.
    pub(crate) fn from_shared(config: SharedCacheConfig) -> Self {
        let mut cache = Self::new(config.root);
        cache.persistent = config.persistent;
        cache.enabled = config.enabled;
        cache
    }

    pub fn new(root: PathBuf) -> Self {
        let clones = clones_files(&root);
        Self {
            root,
            persistent: false,
            _checkout_lease: None,
            native_toolchain: OnceLock::new(),
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

    /// Opt-in experiment: isolate entries by the executable, checkout and
    /// toolchain, including uncommitted source changes.
    pub fn persistent(root: &Path, repository: &Path) -> Result<Self, String> {
        use std::os::unix::ffi::OsStrExt;
        let checkout_id = checkout_id(repository)?;
        fs::create_dir_all(root).map_err(|e| e.to_string())?;
        // Keep this lock outside the removable tree for the life of the run.
        let lease = CacheLock::shared(&root.join(format!("{checkout_id}.lock")))?;
        let mut sha = Sha256::new();
        sha.update(b"l6test-persistent-experiment-v2\0");
        sha.update(crate::hashing::hash(
            &std::env::current_exe().map_err(|e| e.to_string())?,
        )?);
        let files = std::process::Command::new("git")
            .current_dir(repository)
            .args([
                "ls-files",
                "-z",
                "--cached",
                "--others",
                "--exclude-standard",
            ])
            .output()
            .map_err(|e| e.to_string())?;
        if !files.status.success() {
            return Err("cannot enumerate checkout for fixture cache".into());
        }
        for name in files.stdout.split(|b| *b == 0).filter(|s| !s.is_empty()) {
            sha.update((name.len() as u64).to_le_bytes());
            sha.update(name);
            let path = repository.join(std::ffi::OsStr::from_bytes(name));
            if path.is_file() {
                sha.update(crate::hashing::hash(&path)?);
            } else {
                sha.update(b"missing");
            }
        }
        let rustc = std::process::Command::new("rustc")
            .arg("-vV")
            .output()
            .map_err(|e| e.to_string())?;
        if !rustc.status.success() {
            return Err("cannot identify rustc for fixture cache".into());
        }
        sha.update(rustc.stdout);
        let mut environment: Vec<_> = std::env::vars()
            .filter(|(n, _)| {
                n.starts_with("L6_")
                    || n.starts_with("RUST")
                    || n.starts_with("LD_")
                    || n.starts_with("DYLD_")
            })
            .collect();
        environment.sort();
        for (name, value) in environment {
            sha.update(name.as_bytes());
            sha.update([0]);
            sha.update(value.as_bytes());
            sha.update([0]);
        }
        let mut cache = Self::new(root.join(checkout_id).join(format!("{:x}", sha.finalize())));
        cache.persistent = true;
        cache._checkout_lease = Some(lease);
        Ok(cache)
    }

    /// Remove only this checkout's persistent cache. Never wait for a run:
    /// a cached firmware file may still be in use until its lease is dropped.
    pub fn clean(root: &Path, repository: &Path) -> Result<bool, String> {
        let checkout_id = checkout_id(repository)?;
        if !root.exists() {
            return Ok(false);
        }
        let _lease = CacheLock::exclusive_nonblocking(&root.join(format!("{checkout_id}.lock")))?;
        let checkout_root = root.join(checkout_id);
        match fs::symlink_metadata(&checkout_root) {
            Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => {
                fs::remove_dir_all(&checkout_root)
                    .map_err(|e| format!("{}: {e}", checkout_root.display()))?;
                Ok(true)
            }
            Ok(_) => Err(format!(
                "cache path is not a directory: {}",
                checkout_root.display()
            )),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
            Err(error) => Err(format!("{}: {error}", checkout_root.display())),
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
        let toolchain = if self.persistent && kind == "l6patch" {
            let identity = self
                .native_toolchain
                .get_or_init(|| {
                    fs::create_dir_all(&self.root).map_err(|e| e.to_string())?;
                    let _lock = CacheLock::acquire(&self.root.join("toolchain.lock"))?;
                    patch_compile::payload_toolchain_identity(Some(
                        &self.root.join("toolchain.json"),
                    ))
                    .map_err(|e| format!("identify native patch toolchain: {e}"))
                })
                .as_ref()
                .map_err(Clone::clone)?;
            patch_compile::verify_payload_toolchain(identity)
                .map_err(|e| format!("native patch toolchain changed: {e}"))?;
            Some(identity)
        } else {
            None
        };
        let cache_key = match toolchain {
            Some(identity) => format!("{kind}\0{key}\0{}", identity.digest),
            None => format!("{kind}\0{key}"),
        };
        let mut digest = format!("{:x}", Sha256::digest(cache_key.as_bytes()));
        if !self.enabled {
            let n = self.builds.fetch_add(1, Ordering::Relaxed);
            digest = format!(
                "{:x}",
                Sha256::digest(format!("{digest}\0{}\0{n}", std::process::id()).as_bytes())
            );
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
            let name = if self.persistent {
                &digest[..]
            } else {
                &digest[..16]
            };
            let target = directory.join(name);
            let staging = directory.join(format!("{name}.{}.building", std::process::id()));
            fs::create_dir_all(&directory).map_err(|e| e.to_string())?;
            let _lock = if self.enabled {
                Some(CacheLock::acquire(&directory.join(format!("{name}.lock")))?)
            } else {
                None
            };
            let manifest = directory.join(format!("{name}.manifest.json"));
            if self.enabled && target.is_dir() {
                let expected = fs::read(&manifest)
                    .ok()
                    .and_then(|b| serde_json::from_slice::<BTreeMap<String, String>>(&b).ok());
                if expected.is_some_and(|expected| {
                    cache_contents(&target).is_ok_and(|actual| actual == expected)
                }) {
                    if let Some(identity) = toolchain {
                        patch_compile::verify_payload_toolchain(identity)
                            .map_err(|e| format!("native patch toolchain changed: {e}"))?;
                    }
                    built = false;
                    return Ok(target);
                }
                fs::remove_dir_all(&target).map_err(|e| e.to_string())?;
            }
            let _ = fs::remove_dir_all(&staging);
            fs::write(directory.join(format!("{name}.key")), key).map_err(|e| e.to_string())?;
            // Builders such as `l6patch` and `l6fw::unpack` create their
            // output directory, so `build` receives a path that does not exist.
            build(&staging)?;
            if let Some(identity) = toolchain {
                patch_compile::verify_payload_toolchain(identity)
                    .map_err(|e| format!("native patch toolchain changed: {e}"))?;
            }
            fs::create_dir_all(&staging).map_err(|e| e.to_string())?;
            let hashes = if self.enabled {
                Some(cache_contents(&staging)?)
            } else {
                None
            };
            fs::rename(&staging, &target).map_err(|e| e.to_string())?;
            if let Some(hashes) = hashes {
                fs::write(
                    &manifest,
                    serde_json::to_vec(&hashes).map_err(|e| e.to_string())?,
                )
                .map_err(|e| e.to_string())?;
            }
            Ok(target)
        });
        let mut stats = self.stats.lock().unwrap();
        let stats = stats.entry(kind.to_owned()).or_default();
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

    /// Incorporate counters from an isolated, supervised campaign member.
    pub(crate) fn merge_stats(&self, value: &Value) -> Result<(), String> {
        let entries = value.as_object().ok_or("invalid member cache statistics")?;
        let mut stats = self.stats.lock().map_err(|e| e.to_string())?;
        for (kind, counts) in entries {
            let hits = counts["hits"].as_u64().ok_or("invalid member cache hits")?;
            let builds = counts["builds"]
                .as_u64()
                .ok_or("invalid member cache builds")?;
            let entry = stats.entry(kind.clone()).or_default();
            entry.hits = entry.hits.saturating_add(hits);
            entry.builds = entry.builds.saturating_add(builds);
        }
        Ok(())
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

struct CacheLock(fs::File);
impl CacheLock {
    fn acquire(path: &Path) -> Result<Self, String> {
        Self::lock(path, libc::LOCK_EX)
    }
    fn shared(path: &Path) -> Result<Self, String> {
        Self::lock(path, libc::LOCK_SH)
    }
    fn exclusive_nonblocking(path: &Path) -> Result<Self, String> {
        Self::lock(path, libc::LOCK_EX | libc::LOCK_NB)
    }
    fn lock(path: &Path, mode: i32) -> Result<Self, String> {
        use std::os::fd::AsRawFd;
        let file = fs::OpenOptions::new()
            .create(true)
            .write(true)
            .open(path)
            .map_err(|e| e.to_string())?;
        if unsafe { libc::flock(file.as_raw_fd(), mode) } != 0 {
            let error = std::io::Error::last_os_error();
            if error.kind() == std::io::ErrorKind::WouldBlock {
                return Err("cache is in use by an active run; try again after it finishes".into());
            }
            return Err(format!("{}: {error}", path.display()));
        }
        Ok(Self(file))
    }
}

fn checkout_id(repository: &Path) -> Result<String, String> {
    use std::os::unix::ffi::OsStrExt;
    let checkout = repository.canonicalize().map_err(|e| e.to_string())?;
    Ok(format!(
        "{:x}",
        Sha256::digest(checkout.as_os_str().as_bytes())
    ))
}
impl Drop for CacheLock {
    fn drop(&mut self) {
        use std::os::fd::AsRawFd;
        unsafe {
            libc::flock(self.0.as_raw_fd(), libc::LOCK_UN);
        }
    }
}

fn cache_contents(root: &Path) -> Result<BTreeMap<String, String>, String> {
    fn visit(root: &Path, dir: &Path, out: &mut BTreeMap<String, String>) -> Result<(), String> {
        for entry in fs::read_dir(dir).map_err(|e| e.to_string())? {
            let entry = entry.map_err(|e| e.to_string())?;
            let kind = entry.file_type().map_err(|e| e.to_string())?;
            let path = entry.path();
            if kind.is_symlink() {
                return Err(format!("cache output contains symlink: {}", path.display()));
            }
            if kind.is_dir() {
                visit(root, &path, out)?;
            } else if kind.is_file() {
                let hash = if path.extension().is_some_and(|x| x == "img") {
                    crate::hashing::hash_sparse(&path)?
                } else {
                    crate::hashing::hash(&path)?
                };
                out.insert(
                    path.strip_prefix(root)
                        .unwrap()
                        .to_string_lossy()
                        .into_owned(),
                    hash,
                );
            } else {
                return Err(format!("unsupported cache output: {}", path.display()));
            }
        }
        Ok(())
    }
    let mut result = BTreeMap::new();
    visit(root, root, &mut result)?;
    Ok(result)
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
    static NEXT_PROBE: AtomicU64 = AtomicU64::new(0);
    let id = NEXT_PROBE.fetch_add(1, Ordering::Relaxed);
    let name = format!(".clone-probe-{}-{id}", std::process::id());
    let source = root.join(&name);
    let target = root.join(format!("{name}.copy"));
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
    fn supervised_members_share_cached_builds_across_processes() {
        if let Some(root) = std::env::var_os("L6TEST_CACHE_PROCESS_TEST") {
            use std::io::Write;
            let root = PathBuf::from(root);
            let cache = RunCache::new(root.join("cache"));
            cache
                .directory("card", "same-fixture", |path| {
                    fs::create_dir_all(path).map_err(|e| e.to_string())?;
                    let mut marker = fs::OpenOptions::new()
                        .create(true)
                        .append(true)
                        .open(root.join("builds"))
                        .map_err(|e| e.to_string())?;
                    marker.write_all(b"build\n").map_err(|e| e.to_string())?;
                    std::thread::sleep(std::time::Duration::from_millis(150));
                    fs::write(path.join("fixture"), b"cached").map_err(|e| e.to_string())
                })
                .unwrap();
            return;
        }
        let root = temp("process-share");
        fs::create_dir_all(&root).unwrap();
        let spawn = || {
            std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "cache::tests::supervised_members_share_cached_builds_across_processes",
                ])
                .env("L6TEST_CACHE_PROCESS_TEST", &root)
                .stdout(std::process::Stdio::null())
                .spawn()
                .unwrap()
        };
        let mut first = spawn();
        let mut second = spawn();
        assert!(first.wait().unwrap().success());
        assert!(second.wait().unwrap().success());
        assert!(spawn().wait().unwrap().success());
        assert_eq!(fs::read_to_string(root.join("builds")).unwrap(), "build\n");
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn persistent_entries_survive_runs_and_rebuild_after_corruption() {
        let dir = temp("persistent");
        let builds = AtomicUsize::new(0);
        let new_cache = || {
            let mut c = RunCache::new(dir.clone());
            c.persistent = true;
            c
        };
        let build = |path: &Path| {
            builds.fetch_add(1, Ordering::Relaxed);
            fs::create_dir_all(path).map_err(|e| e.to_string())?;
            fs::write(path.join("fixture"), b"expected").map_err(|e| e.to_string())
        };
        let first = new_cache().directory("test", "key", build).unwrap();
        let second = new_cache().directory("test", "key", build).unwrap();
        assert_eq!(first, second);
        assert_eq!(builds.load(Ordering::Relaxed), 1);
        fs::write(first.join("fixture"), b"corrupt").unwrap();
        new_cache().directory("test", "key", build).unwrap();
        assert_eq!(builds.load(Ordering::Relaxed), 2);
        assert_eq!(fs::read(first.join("fixture")).unwrap(), b"expected");
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn changing_nor_or_patching_source_invalidates_persistent_namespace() {
        let dir = temp("nor-fingerprint");
        fs::create_dir_all(dir.join("tools/l6nor/src")).unwrap();
        assert!(
            std::process::Command::new("git")
                .current_dir(&dir)
                .args(["init", "-q"])
                .status()
                .unwrap()
                .success()
        );
        let source = dir.join("tools/l6nor/src/lib.rs");
        fs::create_dir_all(dir.join("tools/l6patch/src")).unwrap();
        let patch_source = dir.join("tools/l6patch/src/lib.rs");
        fs::write(&source, b"before").unwrap();
        fs::write(&patch_source, b"before").unwrap();
        assert!(
            std::process::Command::new("git")
                .current_dir(&dir)
                .args(["add", "tools/l6nor/src/lib.rs", "tools/l6patch/src/lib.rs"])
                .status()
                .unwrap()
                .success()
        );
        let first = RunCache::persistent(&dir.join("cache"), &dir).unwrap().root;
        fs::write(&source, b"after").unwrap();
        let second = RunCache::persistent(&dir.join("cache"), &dir).unwrap().root;
        assert_ne!(first, second);
        fs::write(&patch_source, b"after").unwrap();
        let third = RunCache::persistent(&dir.join("cache"), &dir).unwrap().root;
        assert_ne!(second, third);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn identical_checkouts_have_independent_cache_roots() {
        let dir = temp("checkout-isolation");
        let parent = dir.join("shared-parent");
        let mut roots = Vec::new();
        for name in ["checkout-a", "checkout-b"] {
            let checkout = dir.join(name);
            fs::create_dir_all(&checkout).unwrap();
            assert!(
                std::process::Command::new("git")
                    .current_dir(&checkout)
                    .args(["init", "-q"])
                    .status()
                    .unwrap()
                    .success()
            );
            fs::write(checkout.join("same.rs"), b"same contents").unwrap();
            roots.push(RunCache::persistent(&parent, &checkout).unwrap().root);
        }
        assert_ne!(roots[0].parent(), roots[1].parent());
        let a = RunCache {
            root: roots[0].clone(),
            persistent: true,
            ..RunCache::new(roots[0].clone())
        };
        let b = RunCache {
            root: roots[1].clone(),
            persistent: true,
            ..RunCache::new(roots[1].clone())
        };
        let first = a
            .directory("test", "same", |path| {
                fs::create_dir_all(path).map_err(|e| e.to_string())
            })
            .unwrap();
        let second = b
            .directory("test", "same", |path| {
                fs::create_dir_all(path).map_err(|e| e.to_string())
            })
            .unwrap();
        assert_ne!(first, second);
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn clean_refuses_active_checkout_and_preserves_other_checkouts() {
        let dir = temp("clean-checkout");
        let parent = dir.join("cache");
        let a = dir.join("a");
        let b = dir.join("b");
        for checkout in [&a, &b] {
            fs::create_dir_all(checkout).unwrap();
            assert!(
                std::process::Command::new("git")
                    .current_dir(checkout)
                    .args(["init", "-q"])
                    .status()
                    .unwrap()
                    .success()
            );
            fs::write(checkout.join("source"), b"source").unwrap();
        }
        let a_cache = RunCache::persistent(&parent, &a).unwrap();
        let b_cache = RunCache::persistent(&parent, &b).unwrap();
        for cache in [&a_cache, &b_cache] {
            cache
                .directory("test", "fixture", |path| {
                    fs::create_dir_all(path).map_err(|e| e.to_string())?;
                    fs::write(path.join("fixture"), b"cached").map_err(|e| e.to_string())
                })
                .unwrap();
        }
        let a_root = a_cache.root.clone();
        let b_root = b_cache.root.clone();
        let err = RunCache::clean(&parent, &a).unwrap_err();
        assert!(err.contains("active run"), "{err}");
        assert!(a_root.exists());
        drop(a_cache);
        assert!(RunCache::clean(&parent, &a).unwrap());
        assert!(!a_root.exists());
        assert!(b_root.exists());
        assert!(!RunCache::clean(&parent, &a).unwrap());
        drop(b_cache);
        fs::remove_dir_all(dir).unwrap();
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
