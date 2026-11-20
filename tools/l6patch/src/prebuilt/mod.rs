//! `l6patch prebuilt`: the committed files built from patch sources and stock
//! firmware. These are the browser patcher's templates and the SD modules the
//! website and the emulator serve. Each manifest records a source
//! fingerprint, and a test fails while they are stale.
mod fingerprint;
mod modules;
mod templates;

use std::{
    fs,
    path::{Path, PathBuf},
};

/// Where the stock firmware comes from.
pub enum Stock {
    /// The `[firmware]` packages of a `config.toml`.
    Config(PathBuf),
    /// Unpacked stock folders, as `l6fw unpack` writes them.
    Unpacked { l6max: PathBuf, l6: PathBuf },
}

/// The repository root this `l6patch` was built from.
pub fn repository() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// Regenerate every prebuilt file.
pub fn generate(stock: Stock) -> Result<(), Box<dyn std::error::Error>> {
    let root = repository().canonicalize()?;
    let scratch = std::env::temp_dir().join(format!("l6patch-prebuilt-{}", std::process::id()));
    if scratch.exists() {
        fs::remove_dir_all(&scratch)?;
    }
    fs::create_dir_all(&scratch)?;
    let (l6max, l6) = match stock {
        Stock::Unpacked { l6max, l6 } => (l6max, l6),
        Stock::Config(config) => {
            let packages = config_packages(&config)?;
            let mut unpacked = Vec::new();
            for (model, package) in [("l6max", &packages.0), ("l6", &packages.1)] {
                let directory = scratch.join(format!("stock-{model}"));
                l6fw::unpack(package, &directory)?;
                unpacked.push(directory);
            }
            let l6 = unpacked.pop().unwrap();
            (unpacked.pop().unwrap(), l6)
        }
    };
    let fingerprint = fingerprint::source_fingerprint(&root)?;
    templates::generate(&l6max, &l6, &fingerprint)?;
    modules::generate(&root, &l6max, &l6, &scratch.join("modules"), &fingerprint)?;
    fs::remove_dir_all(&scratch)?;
    Ok(())
}

/// Check the committed prebuilt files against the current sources, without
/// firmware.
pub fn check() -> Result<(), String> {
    fingerprint::check(&repository().canonicalize().map_err(|e| e.to_string())?)
}

/// The L6max and L6 package paths from a `config.toml` `[firmware]` table.
/// Relative paths follow the file, as in `config.example.toml`.
fn config_packages(config: &Path) -> Result<(PathBuf, PathBuf), String> {
    let text = fs::read_to_string(config).map_err(|e| format!("{}: {e}", config.display()))?;
    let document: toml::Table =
        toml::from_str(&text).map_err(|e| format!("{}: {e}", config.display()))?;
    let base = config.parent().unwrap_or(Path::new("."));
    let path = |model: &str| {
        document
            .get("firmware")
            .and_then(|firmware| firmware.get(model))
            .and_then(toml::Value::as_str)
            .map(|path| base.join(path))
            .ok_or_else(|| {
                format!(
                    "{} has no [firmware] {model} package; prebuilt files need both models",
                    config.display()
                )
            })
    };
    Ok((path("l6max")?, path("l6")?))
}
