//! The web patcher hosts prebuilt SD patch modules. They are generated with
//! `l6patch prebuilt`, which needs stock firmware, so CI
//! cannot rebuild them; it can check that the current loader accepts them.
use patch_abi::dynamic::{self, Module};
use sha2::{Digest, Sha256};
use std::{fs, path::Path};

#[test]
fn hosted_modules_load_with_the_current_loader() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../web/public/loadable-patches");
    let manifest: serde_json::Value =
        serde_json::from_slice(&fs::read(root.join("manifest.json")).unwrap()).unwrap();
    let modules = manifest["modules"].as_array().unwrap();
    let effects = manifest["effect_replacements"].as_array().unwrap();
    assert!(!modules.is_empty());
    assert!(!effects.is_empty());
    for module in modules.iter().chain(effects.iter()) {
        let file = module["file"].as_str().unwrap();
        let bytes = fs::read(root.join(file)).unwrap();
        assert_eq!(
            format!("{:x}", Sha256::digest(&bytes)),
            module["sha256"].as_str().unwrap(),
            "{file}: manifest hash"
        );
        assert_eq!(
            module["format_version"].as_u64(),
            Some(dynamic::FORMAT_VERSION.into()),
            "{file}: manifest format version"
        );
        assert_eq!(
            module["loader_abi"].as_u64(),
            Some(dynamic::LOADER_ABI.into()),
            "{file}: manifest loader ABI"
        );

        let decoded = Module::decode(&bytes)
            .unwrap_or_else(|error| panic!("{file}: loader rejects it: {error:?}"));
        assert_eq!(
            decoded.encode().as_deref(),
            Ok(&bytes[..]),
            "{file}: the writer encodes it differently"
        );
    }

    // Every hosted module is listed, so none can be served unchecked.
    let mut hosted = Vec::new();
    for model in fs::read_dir(&root).unwrap().flatten() {
        if model.path().is_dir() {
            for file in fs::read_dir(model.path()).unwrap().flatten() {
                let name = file.file_name().into_string().unwrap();
                hosted.push(format!("{}/{name}", model.file_name().to_string_lossy()));
            }
        }
    }
    hosted.sort();
    let mut listed: Vec<String> = modules
        .iter()
        .chain(effects.iter())
        .map(|module| module["file"].as_str().unwrap().to_owned())
        .collect();
    listed.sort();
    assert_eq!(hosted, listed);
}
