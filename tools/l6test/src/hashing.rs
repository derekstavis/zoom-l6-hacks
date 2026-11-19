//! Content hashes of files, trees and sparse images, and canonical digests of
//! JSON values that do not depend on map key order.
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    fs,
    io::{Read, Seek},
    path::{Path, PathBuf},
};

pub(crate) fn hash(path: &Path) -> Result<String, String> {
    let mut file = fs::File::open(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let mut sha = Sha256::new();
    let mut buffer = [0; 65536];
    loop {
        let n = file.read(&mut buffer).map_err(|e| e.to_string())?;
        if n == 0 {
            break;
        }
        sha.update(&buffer[..n]);
    }
    Ok(format!("{:x}", sha.finalize()))
}
pub(crate) fn hash_patch_directory(path: &Path) -> Result<String, String> {
    let mut entries = fs::read_dir(path)
        .map_err(|e| format!("{}: {e}", path.display()))?
        .map(|entry| entry.map_err(|e| e.to_string()))
        .collect::<Result<Vec<_>, _>>()?;
    entries.sort_by_key(|entry| entry.file_name());
    let mut sha = Sha256::new();
    for entry in entries {
        if !entry.file_type().map_err(|e| e.to_string())?.is_file() {
            continue;
        }
        let name = entry.file_name();
        let name = name.to_string_lossy();
        sha.update((name.len() as u32).to_le_bytes());
        sha.update(name.as_bytes());
        let mut file = fs::File::open(entry.path()).map_err(|e| e.to_string())?;
        let mut buffer = [0; 65536];
        loop {
            let count = file.read(&mut buffer).map_err(|e| e.to_string())?;
            if count == 0 {
                break;
            }
            sha.update(&buffer[..count]);
        }
    }
    Ok(format!("{:x}", sha.finalize()))
}
/// Hash a sparse card image by its allocated extents and total length, so a
/// large image costs only what it stores. Compare only hashes from this function.
pub(crate) fn hash_sparse(path: &Path) -> Result<String, String> {
    use std::os::fd::AsRawFd;
    let mut file = fs::File::open(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let length = file.metadata().map_err(|e| e.to_string())?.len();
    let fd = file.as_raw_fd();
    let mut sha = Sha256::new();
    sha.update(length.to_le_bytes());
    let mut buffer = vec![0; 1024 * 1024];
    let mut position = 0;
    while position < length {
        let data = unsafe { libc::lseek(fd, position as libc::off_t, libc::SEEK_DATA) };
        if data < 0 {
            let error = std::io::Error::last_os_error();
            if error.raw_os_error() == Some(libc::ENXIO) {
                break;
            }
            return Err(format!("{}: {error}", path.display()));
        }
        let hole = unsafe { libc::lseek(fd, data, libc::SEEK_HOLE) };
        if hole < 0 {
            return Err(format!(
                "{}: {}",
                path.display(),
                std::io::Error::last_os_error()
            ));
        }
        file.seek(std::io::SeekFrom::Start(data as u64))
            .map_err(|e| e.to_string())?;
        sha.update((data as u64).to_le_bytes());
        let mut remaining = (hole - data) as u64;
        while remaining > 0 {
            let count = remaining.min(buffer.len() as u64) as usize;
            file.read_exact(&mut buffer[..count])
                .map_err(|e| e.to_string())?;
            sha.update(&buffer[..count]);
            remaining -= count as u64;
        }
        position = hole as u64;
    }
    Ok(format!("{:x}", sha.finalize()))
}
pub(crate) fn files(root: &Path) -> Result<BTreeMap<String, String>, String> {
    fn visit(
        root: &Path,
        path: &Path,
        output: &mut BTreeMap<String, String>,
    ) -> Result<(), String> {
        for entry in fs::read_dir(path).map_err(|e| e.to_string())? {
            let path = entry.map_err(|e| e.to_string())?.path();
            if path.is_symlink() {
                return Err(format!(
                    "fixture/source tree contains a symlink: {}",
                    path.display()
                ));
            }
            if path.is_dir() {
                visit(root, &path, output)?;
            } else {
                output.insert(
                    path.strip_prefix(root).unwrap().to_string_lossy().into(),
                    hash(&path)?,
                );
            }
        }
        Ok(())
    }
    let mut result = BTreeMap::new();
    visit(root, root, &mut result)?;
    Ok(result)
}

/// `value` serialized with object keys sorted at every level, so the text does
/// not depend on insertion order (`serde_json`'s `preserve_order` feature is
/// enabled in some builds).
pub(crate) fn canonical(value: &Value) -> String {
    fn sorted(value: &Value) -> Value {
        match value {
            Value::Object(map) => Value::Object(
                map.iter()
                    .map(|(k, v)| (k.clone(), sorted(v)))
                    .collect::<BTreeMap<_, _>>()
                    .into_iter()
                    .collect(),
            ),
            Value::Array(items) => Value::Array(items.iter().map(sorted).collect()),
            other => other.clone(),
        }
    }
    let mut text = String::new();
    write_sorted(&sorted(value), &mut text);
    text
}

fn write_sorted(value: &Value, out: &mut String) {
    match value {
        Value::Object(map) => {
            let mut entries: Vec<_> = map.iter().collect();
            entries.sort_by(|a, b| a.0.cmp(b.0));
            out.push('{');
            for (i, (key, value)) in entries.into_iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                out.push_str(&Value::String(key.clone()).to_string());
                out.push(':');
                write_sorted(value, out);
            }
            out.push('}');
        }
        Value::Array(items) => {
            out.push('[');
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                write_sorted(item, out);
            }
            out.push(']');
        }
        other => out.push_str(&other.to_string()),
    }
}

/// SHA-256 of the canonical form of `value`.
pub(crate) fn digest(value: &Value) -> String {
    format!("{:x}", Sha256::digest(canonical(value).as_bytes()))
}

/// Digest of files by name and content, independent of where they live.
pub(crate) fn digest_files(files: &[(String, PathBuf)]) -> Result<String, String> {
    let mut sha = Sha256::new();
    for (name, path) in files {
        sha.update((name.len() as u64).to_le_bytes());
        sha.update(name.as_bytes());
        sha.update(hash(path)?.as_bytes());
    }
    Ok(format!("{:x}", sha.finalize()))
}

/// Digest of every file under `root`, or of nothing if it does not exist.
pub(crate) fn tree_digest(root: &Path) -> Result<String, String> {
    if !root.exists() {
        return Ok(digest(&Value::Null));
    }
    Ok(digest(&serde_json::json!(files(root)?)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn canonical_text_ignores_key_order() {
        let mut a = serde_json::Map::new();
        a.insert("b".into(), json!(1));
        a.insert("a".into(), json!({"y": [1, {"d": 1, "c": 2}], "x": null}));
        let mut b = serde_json::Map::new();
        b.insert("a".into(), json!({"x": null, "y": [1, {"c": 2, "d": 1}]}));
        b.insert("b".into(), json!(1));
        let (a, b) = (Value::Object(a), Value::Object(b));
        assert_eq!(canonical(&a), canonical(&b));
        assert_eq!(
            canonical(&a),
            r#"{"a":{"x":null,"y":[1,{"c":2,"d":1}]},"b":1}"#
        );
        assert_eq!(digest(&a), digest(&b));
    }
}
