//! Content identity of the native payload compiler and its inputs.
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeSet,
    fs,
    io::{self, Read},
    path::{Path, PathBuf},
    process::Command,
};

fn add_file(files: &mut BTreeSet<PathBuf>, path: &Path) -> io::Result<()> {
    let resolved = path.canonicalize()?;
    if !resolved.is_file() {
        return Err(io::Error::other(format!("not a file: {}", path.display())));
    }
    files.insert(path.to_path_buf());
    files.insert(resolved);
    Ok(())
}

fn add_tree(
    files: &mut BTreeSet<PathBuf>,
    dirs: &mut BTreeSet<PathBuf>,
    path: &Path,
) -> io::Result<()> {
    dirs.insert(path.canonicalize()?);
    for entry in fs::read_dir(path)? {
        let entry = entry?;
        let kind = entry.file_type()?;
        if kind.is_symlink() {
            let resolved = entry.path().canonicalize()?;
            if resolved.is_dir() {
                return Err(io::Error::other(format!(
                    "directory symlink in compiler source: {}",
                    entry.path().display()
                )));
            }
            add_file(files, &entry.path())?;
        } else if kind.is_dir() {
            add_tree(files, dirs, &entry.path())?;
        } else if kind.is_file() {
            add_file(files, &entry.path())?;
        }
    }
    Ok(())
}

fn output(command: &mut Command) -> io::Result<String> {
    let result = command.output()?;
    if !result.status.success() {
        return Err(io::Error::other(format!(
            "{command:?}: {}",
            String::from_utf8_lossy(&result.stderr)
        )));
    }
    String::from_utf8(result.stdout).map_err(io::Error::other)
}

/// Compiler binary, compiler libraries, and all rust-src library inputs used
/// by the immediate-abort core and compiler-builtins build. This identity also
/// invalidates that build's own cache when a toolchain is replaced in place.
fn compiler_inputs(rustc: &Path) -> io::Result<(BTreeSet<PathBuf>, BTreeSet<PathBuf>)> {
    let sysroot = PathBuf::from(output(Command::new(rustc).args(["--print", "sysroot"]))?.trim());
    let mut files = BTreeSet::new();
    let mut dirs = BTreeSet::new();
    add_file(&mut files, rustc)?;
    add_tree(
        &mut files,
        &mut dirs,
        &sysroot.join("lib/rustlib/src/rust/library"),
    )?;
    dirs.insert(sysroot.join("lib").canonicalize()?);
    for entry in fs::read_dir(sysroot.join("lib"))? {
        let entry = entry?;
        let name = entry.file_name().to_string_lossy().into_owned();
        if name.starts_with("librustc_driver") || name.starts_with("libLLVM") {
            add_file(&mut files, &entry.path())?;
        }
    }
    Ok((files, dirs))
}

pub fn compiler_identity(rustc: &Path) -> io::Result<String> {
    use std::sync::{Mutex, OnceLock};
    static CACHE: OnceLock<Mutex<Option<(PathBuf, PayloadToolchain)>>> = OnceLock::new();
    let mut cache = CACHE.get_or_init(|| Mutex::new(None)).lock().unwrap();
    if let Some((path, identity)) = &*cache {
        if path == rustc && identity.verify().is_ok() {
            return Ok(identity.digest.clone());
        }
    }
    let (files, dirs) = compiler_inputs(rustc)?;
    let stamps = files
        .iter()
        .chain(&dirs)
        .map(|path| Ok((path.clone(), stamp(path)?)))
        .collect::<io::Result<Vec<_>>>()?;
    let identity = PayloadToolchain {
        digest: hash_files(&files)?,
        stamps,
        tools: vec![],
        loader_environment: None,
        search_dirs: vec![],
    };
    identity.verify()?;
    let digest = identity.digest.clone();
    *cache = Some((rustc.to_path_buf(), identity));
    Ok(digest)
}

/// A content digest and the filesystem objects observed while computing it.
/// Verification detects replacement, modification and source-tree additions
/// while a run is using that identity.
#[derive(serde::Serialize, serde::Deserialize)]
pub struct PayloadToolchain {
    pub digest: String,
    stamps: Vec<(PathBuf, Stamp)>,
    tools: Vec<(PathBuf, PathBuf)>,
    loader_environment: Option<Vec<(String, String)>>,
    search_dirs: Vec<(PathBuf, Vec<SearchEntry>)>,
}

type SearchEntry = (PathBuf, u8, Option<PathBuf>);

#[derive(PartialEq, Eq, serde::Serialize, serde::Deserialize)]
struct Stamp {
    device: u64,
    inode: u64,
    size: u64,
    modified: i64,
    modified_ns: i64,
    changed: i64,
    changed_ns: i64,
}

fn stamp(path: &Path) -> io::Result<Stamp> {
    use std::os::unix::fs::MetadataExt;
    let m = fs::metadata(path)?;
    Ok(Stamp {
        device: m.dev(),
        inode: m.ino(),
        size: m.size(),
        modified: m.mtime(),
        modified_ns: m.mtime_nsec(),
        changed: m.ctime(),
        changed_ns: m.ctime_nsec(),
    })
}

impl PayloadToolchain {
    pub fn verify_tools(&self, tools: &[PathBuf]) -> io::Result<()> {
        if tools.len() != self.tools.len() {
            return Err(io::Error::other("compiler tool set changed during run"));
        }
        for (current, (_, expected)) in tools.iter().zip(&self.tools) {
            if current.canonicalize()? != *expected {
                return Err(io::Error::other(format!(
                    "selected compiler tool changed: {}",
                    current.display()
                )));
            }
        }
        self.verify()
    }

    pub fn verify(&self) -> io::Result<()> {
        if let Some(expected) = &self.loader_environment {
            if &loader_environment()? != expected {
                return Err(io::Error::other(
                    "dynamic loader environment changed during run",
                ));
            }
        }
        for (path, expected) in &self.tools {
            if path.canonicalize()? != *expected {
                return Err(io::Error::other(format!(
                    "compiler tool was replaced: {}",
                    path.display()
                )));
            }
        }
        for (path, expected) in &self.stamps {
            if stamp(path)? != *expected {
                return Err(io::Error::other(format!(
                    "compiler input changed during run: {}",
                    path.display()
                )));
            }
        }
        for (path, expected) in &self.search_dirs {
            if &directory_entries(path)? != expected {
                return Err(io::Error::other(format!(
                    "loader search directory changed: {}",
                    path.display()
                )));
            }
        }
        Ok(())
    }
}

fn hash_files(files: &BTreeSet<PathBuf>) -> io::Result<String> {
    let mut hash = Sha256::new();
    let mut buf = [0; 65536];
    for path in files {
        let name = path.to_string_lossy();
        hash.update((name.len() as u64).to_le_bytes());
        hash.update(name.as_bytes());
        let mut file = fs::File::open(path)?;
        let length = file.metadata()?.len();
        hash.update(length.to_le_bytes());
        loop {
            let read = file.read(&mut buf)?;
            if read == 0 {
                break;
            }
            hash.update(&buf[..read]);
        }
    }
    Ok(format!("{:x}", hash.finalize()))
}

/// Identify the actual native tools, their dynamically loaded libraries, and
/// the Rust source and compiler used to construct the payload sysroot.
pub fn payload_identity(
    rustc: &Path,
    tools: &[PathBuf],
    artifacts: &[PathBuf],
) -> io::Result<PayloadToolchain> {
    // Loader interposition and search overrides make static dependency
    // inspection incomplete. Do not use the persistent cache in that case.
    let environment = loader_environment()?;
    let (mut files, mut dirs) = compiler_inputs(rustc)?;
    let mut search_dirs = BTreeSet::new();
    for name in ["DYLD_FALLBACK_LIBRARY_PATH", "LD_LIBRARY_PATH"] {
        if let Some(paths) = std::env::var_os(name) {
            for path in std::env::split_paths(&paths) {
                track_directory(&mut search_dirs, &path)?;
            }
        }
    }
    let roots = tools
        .iter()
        .map(|path| Ok((path.clone(), path.canonicalize()?)))
        .collect::<io::Result<Vec<_>>>()?;
    for path in tools {
        add_file(&mut files, path)?;
    }
    for path in artifacts {
        add_file(&mut files, path)?;
    }
    add_dynamic_dependencies(&mut files, &mut dirs, &mut search_dirs, tools)?;
    let search_dirs = search_dirs
        .into_iter()
        .map(|path| Ok((path.clone(), directory_entries(&path)?)))
        .collect::<io::Result<Vec<_>>>()?;
    let stamps = files
        .iter()
        .chain(&dirs)
        .map(|path| Ok((path.clone(), stamp(path)?)))
        .collect::<io::Result<Vec<_>>>()?;
    let mut hash = Sha256::new();
    hash.update(b"l6-payload-toolchain-v1\0");
    for name in ["DYLD_FALLBACK_LIBRARY_PATH", "LD_LIBRARY_PATH"] {
        hash.update(name.as_bytes());
        hash.update(
            std::env::var_os(name)
                .unwrap_or_default()
                .to_string_lossy()
                .as_bytes(),
        );
    }
    hash.update(hash_files(&files)?);
    let identity = PayloadToolchain {
        digest: format!("{:x}", hash.finalize()),
        stamps,
        tools: roots,
        loader_environment: Some(environment),
        search_dirs,
    };
    identity.verify()?;
    Ok(identity)
}

fn loader_environment() -> io::Result<Vec<(String, String)>> {
    let mut result = Vec::new();
    for (name, value) in std::env::vars_os() {
        let name = name.to_string_lossy();
        if name.starts_with("LD_") || name.starts_with("DYLD_") {
            let value = value
                .to_str()
                .ok_or_else(|| io::Error::other("non-UTF-8 loader environment"))?;
            if name != "DYLD_FALLBACK_LIBRARY_PATH"
                && name != "LD_LIBRARY_PATH"
                && !value.is_empty()
            {
                return Err(io::Error::other(format!(
                    "cannot safely identify compiler dependencies with {name} set"
                )));
            }
            result.push((name.into_owned(), value.to_owned()));
        }
    }
    result.sort();
    Ok(result)
}

fn track_directory(dirs: &mut BTreeSet<PathBuf>, path: &Path) -> io::Result<()> {
    let mut candidate = path;
    while !candidate.is_dir() {
        candidate = candidate
            .parent()
            .ok_or_else(|| io::Error::other("invalid loader search path"))?;
    }
    dirs.insert(candidate.canonicalize()?);
    Ok(())
}

fn directory_entries(path: &Path) -> io::Result<Vec<SearchEntry>> {
    let mut entries = fs::read_dir(path)?
        .map(|entry| {
            let entry = entry?;
            let kind = entry.file_type()?;
            let target = if kind.is_symlink() {
                Some(fs::read_link(entry.path())?)
            } else {
                None
            };
            Ok((
                PathBuf::from(entry.file_name()),
                if kind.is_symlink() {
                    1
                } else if kind.is_dir() {
                    2
                } else if kind.is_file() {
                    3
                } else {
                    4
                },
                target,
            ))
        })
        .collect::<io::Result<Vec<_>>>()?;
    entries.sort();
    Ok(entries)
}

#[cfg(target_os = "linux")]
fn add_dynamic_dependencies(
    files: &mut BTreeSet<PathBuf>,
    _dirs: &mut BTreeSet<PathBuf>,
    search_dirs: &mut BTreeSet<PathBuf>,
    tools: &[PathBuf],
) -> io::Result<()> {
    for tool in tools {
        let result = Command::new("ldd").arg(tool).output()?;
        let text = format!(
            "{}\n{}",
            String::from_utf8_lossy(&result.stdout),
            String::from_utf8_lossy(&result.stderr)
        );
        if !result.status.success()
            && !text.contains("not a dynamic executable")
            && !text.contains("statically linked")
        {
            return Err(io::Error::other(format!("ldd {}: {text}", tool.display())));
        }
        for line in text.lines() {
            if line.contains("not found") {
                return Err(io::Error::other(format!(
                    "unresolved dependency of {}: {line}",
                    tool.display()
                )));
            }
            let words: Vec<_> = line.split_whitespace().collect();
            let path = if words.get(1) == Some(&"=>") {
                words.get(2)
            } else {
                words.first()
            };
            if let Some(path) = path.filter(|p| p.starts_with('/')) {
                add_file(files, Path::new(path))?;
                if let Some(parent) = Path::new(path).parent() {
                    track_directory(search_dirs, parent)?;
                }
            }
        }
    }
    if Path::new("/etc/ld.so.cache").is_file() {
        add_file(files, Path::new("/etc/ld.so.cache"))?;
    }
    Ok(())
}

#[cfg(target_os = "macos")]
fn add_dynamic_dependencies(
    files: &mut BTreeSet<PathBuf>,
    dirs: &mut BTreeSet<PathBuf>,
    search_dirs: &mut BTreeSet<PathBuf>,
    tools: &[PathBuf],
) -> io::Result<()> {
    // Hash the system shared cache for dependencies that exist only inside
    // dyld's cache, and resolve non-system dependencies using LC_RPATH.
    let system_cache = [
        "/System/Volumes/Preboot/Cryptexes/OS/System/Library/dyld",
        "/System/Library/dyld",
    ]
    .iter()
    .map(PathBuf::from)
    .find(|p| p.is_dir())
    .ok_or_else(|| io::Error::other("cannot locate macOS dyld shared cache"))?;
    let mut found_system_cache = false;
    dirs.insert(system_cache.canonicalize()?);
    for entry in fs::read_dir(system_cache)? {
        let entry = entry?;
        if entry
            .file_name()
            .to_string_lossy()
            .starts_with("dyld_shared_cache_")
            && entry.file_type()?.is_file()
        {
            add_file(files, &entry.path())?;
            found_system_cache = true;
        }
    }
    if !found_system_cache {
        return Err(io::Error::other("macOS dyld shared cache is empty"));
    }
    for tool in tools {
        let executable = tool.canonicalize()?;
        let mut pending = vec![(executable.clone(), Vec::<PathBuf>::new())];
        let mut visited = BTreeSet::new();
        while let Some((file, inherited)) = pending.pop() {
            if !visited.insert(file.clone()) {
                continue;
            }
            let loader = file
                .parent()
                .ok_or_else(|| io::Error::other("compiler tool has no parent"))?;
            let exec_dir = executable.parent().unwrap();
            let expand = |s: &str| -> PathBuf {
                PathBuf::from(
                    s.replace("@loader_path", &loader.to_string_lossy())
                        .replace("@executable_path", &exec_dir.to_string_lossy()),
                )
            };
            let load_commands = output(Command::new("otool").args(["-l"]).arg(&file))?;
            let mut rpaths = inherited.clone();
            let mut in_rpath = false;
            for line in load_commands.lines() {
                let line = line.trim();
                if line == "cmd LC_RPATH" {
                    in_rpath = true;
                } else if in_rpath && line.starts_with("path ") {
                    let value = line
                        .strip_prefix("path ")
                        .unwrap()
                        .split(" (offset ")
                        .next()
                        .unwrap();
                    let path = expand(value);
                    if !path.is_absolute() {
                        return Err(io::Error::other(format!(
                            "unresolved LC_RPATH {value} in {}",
                            file.display()
                        )));
                    }
                    track_directory(search_dirs, &path)?;
                    rpaths.push(path);
                    in_rpath = false;
                }
            }
            let dependencies = output(Command::new("otool").args(["-L"]).arg(&file))?;
            for line in dependencies.lines().skip(1) {
                let name = line
                    .trim()
                    .split(" (compatibility version ")
                    .next()
                    .unwrap_or("");
                if (name.starts_with("/usr/lib/") || name.starts_with("/System/Library/"))
                    && !Path::new(name).is_file()
                {
                    continue;
                }
                let resolved = if let Some(suffix) = name.strip_prefix("@rpath/") {
                    rpaths
                        .iter()
                        .cloned()
                        .chain(
                            std::env::var_os("DYLD_FALLBACK_LIBRARY_PATH")
                                .into_iter()
                                .flat_map(|paths| {
                                    std::env::split_paths(&paths).collect::<Vec<_>>()
                                }),
                        )
                        .map(|p| p.join(suffix))
                        .find(|p| p.is_file())
                        .ok_or_else(|| {
                            io::Error::other(format!(
                                "unresolved dependency {name} in {}",
                                file.display()
                            ))
                        })?
                } else {
                    expand(name)
                };
                if !resolved.is_absolute() {
                    return Err(io::Error::other(format!(
                        "unresolved dependency {name} in {}",
                        file.display()
                    )));
                }
                add_file(files, &resolved)?;
                let resolved = resolved.canonicalize()?;
                if resolved == file {
                    continue;
                }
                pending.push((resolved, rpaths.clone()));
            }
        }
    }
    Ok(())
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn add_dynamic_dependencies(
    _files: &mut BTreeSet<PathBuf>,
    _dirs: &mut BTreeSet<PathBuf>,
    _search_dirs: &mut BTreeSet<PathBuf>,
    _tools: &[PathBuf],
) -> io::Result<()> {
    Err(io::Error::other(
        "persistent native-patch caching is unsupported on this OS",
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snapshot_detects_changed_files_and_added_sources() {
        let root =
            std::env::temp_dir().join(format!("l6-toolchain-snapshot-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        let source = root.join("source.rs");
        fs::write(&source, b"first").unwrap();
        let mut files = BTreeSet::new();
        let mut dirs = BTreeSet::new();
        add_tree(&mut files, &mut dirs, &root).unwrap();
        let digest = hash_files(&files).unwrap();
        let snapshot = PayloadToolchain {
            digest: digest.clone(),
            stamps: files
                .iter()
                .chain(&dirs)
                .map(|p| (p.clone(), stamp(p).unwrap()))
                .collect(),
            tools: vec![],
            loader_environment: None,
            search_dirs: vec![(root.clone(), directory_entries(&root).unwrap())],
        };
        snapshot.verify().unwrap();
        let encoded = serde_json::to_vec(&snapshot).unwrap();
        let restored: PayloadToolchain = serde_json::from_slice(&encoded).unwrap();
        restored.verify().unwrap();
        assert_eq!(restored.digest, digest);
        fs::write(&source, b"other").unwrap();
        assert!(restored.verify().is_err());
        assert_ne!(digest, hash_files(&files).unwrap());
        let dir_stamp = stamp(&root).unwrap();
        fs::write(root.join("added.rs"), b"new").unwrap();
        assert!(dir_stamp != stamp(&root).unwrap());
        fs::remove_dir_all(root).unwrap();
    }
}
