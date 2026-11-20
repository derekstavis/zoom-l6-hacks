//! Local configuration: firmware packages, QEMU and the packaged app.
use crate::hashing::hash;
use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
};

/// What the driver runs with: the configuration's firmware packages, and the QEMU
/// and packaged app chosen for this run.
pub(crate) struct Config {
    pub(crate) firmware: BTreeMap<String, PathBuf>,
    pub(crate) qemu: PathBuf,
    pub(crate) qemu_sha256: String,
    pub(crate) app: Option<App>,
}
/// A packaged application, for the cases that test one.
pub(crate) struct App {
    pub(crate) executable: PathBuf,
    pub(crate) qemu_sha256: String,
}
/// Use the requested QEMU, or the repository's own, building it if `build` is set
/// and it is missing or older than its sources.
pub(crate) fn resolve_qemu(
    root: &Path,
    build: bool,
    requested: Option<&Path>,
) -> Result<PathBuf, String> {
    if let Some(requested) = requested {
        return fs::canonicalize(requested).map_err(|e| format!("{}: {e}", requested.display()));
    }
    let binary = qemu_build::default_binary(root);
    if build && qemu_build::is_stale(root, &binary) {
        qemu_build::build_locked(&qemu_build::Request {
            repository: root.to_owned(),
            source: None,
            minimal: false,
            plugins: false,
            optimization: qemu_build::Optimization::Standard,
        })?;
    }
    if !binary.is_file() {
        return Err(format!(
            "{} is missing; run without --no-build or pass --qemu",
            binary.display()
        ));
    }
    Ok(binary)
}

/// Find the executable and QEMU inside a macOS `.app` or a Linux AppDir.
pub(crate) fn resolve_app(path: &Path) -> Result<App, String> {
    let (executable, qemu) = if path.join("Contents").is_dir() {
        (
            path.join("Contents/MacOS/l6max-gui"),
            path.join("Contents/Resources/libexec/qemu-system-arm"),
        )
    } else {
        (
            path.join("usr/bin/l6max-gui"),
            path.join("usr/share/l6max-emulator/libexec/qemu-system-arm"),
        )
    };
    if !executable.is_file() || !qemu.is_file() {
        return Err(format!("{} is not a packaged application", path.display()));
    }
    Ok(App {
        executable: fs::canonicalize(executable).map_err(|e| e.to_string())?,
        qemu_sha256: hash(&qemu)?,
    })
}

pub(crate) fn load_config(
    path: &Path,
    root: &Path,
    build: bool,
    qemu: Option<&Path>,
    app: Option<&Path>,
) -> Result<Config, String> {
    let path = fs::canonicalize(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let firmware = l6max_host::config::Config::load(&path)?.firmware;
    for (model, firmware) in &firmware {
        if !firmware.is_file() {
            return Err(format!(
                "firmware for {model} must be an update package: {}",
                firmware.display()
            ));
        }
    }
    let qemu = resolve_qemu(root, build, qemu)?;
    Ok(Config {
        firmware,
        qemu_sha256: hash(&qemu)?,
        qemu,
        app: app.map(resolve_app).transpose()?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use sha2::{Digest, Sha256};
    use std::{
        env,
        time::{SystemTime, UNIX_EPOCH},
    };
    static NEXT_TEMP: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    fn temp() -> PathBuf {
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = env::temp_dir().join(format!(
            "l6test-unit-{}-{stamp}-{}",
            std::process::id(),
            NEXT_TEMP.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        fs::create_dir_all(&path).unwrap();
        path
    }

    #[test]
    fn firmware_must_be_a_package_and_fixtures_are_not_local() {
        let root = l6max_host::paths::repository_root();
        let dir = temp();
        let qemu = dir.join("qemu");
        fs::write(&qemu, b"qemu").unwrap();
        fs::write(dir.join("L6max.bin"), b"").unwrap();
        fs::create_dir(dir.join("unpacked")).unwrap();
        let config = dir.join("local.toml");
        let write = |body: &str| fs::write(&config, format!("version=1\n{body}")).unwrap();
        let load = || load_config(&config, &root, false, Some(&qemu), None);
        write("[firmware]\nl6max='L6max.bin'\n");
        assert!(load().is_ok());
        write("[firmware]\nl6max='L6max.bin'\n[emulator.l6max.input_patch]\n1='/missing.wav'\n");
        assert!(load().is_ok(), "the driver ignores input patches");
        write("[firmware]\nl6max='unpacked'\n");
        assert!(load().err().unwrap().contains("update package"));
        write("[firmware]\nother='L6max.bin'\n");
        assert!(load().err().unwrap().contains("unknown model"));
        write("[[fixture]]\nname='prepared'\nmodel='l6max'\n");
        assert!(load().is_err());
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_packaged_app_needs_its_executable_and_qemu() {
        let dir = temp();
        assert!(resolve_app(&dir).is_err());
        fs::create_dir_all(dir.join("Contents/MacOS")).unwrap();
        fs::create_dir_all(dir.join("Contents/Resources/libexec")).unwrap();
        fs::write(dir.join("Contents/MacOS/l6max-gui"), b"gui").unwrap();
        fs::write(
            dir.join("Contents/Resources/libexec/qemu-system-arm"),
            b"qemu",
        )
        .unwrap();
        let app = resolve_app(&dir).unwrap();
        assert!(app.executable.ends_with("Contents/MacOS/l6max-gui"));
        assert_eq!(app.qemu_sha256, format!("{:x}", Sha256::digest(b"qemu")));
        fs::remove_dir_all(dir).unwrap();
    }
}
