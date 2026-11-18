use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    env, fs,
    path::PathBuf,
};

fn main() {
    let manifest_dir = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").unwrap());
    let patch_root = manifest_dir.join("../../patches");
    println!("cargo:rerun-if-changed={}", patch_root.display());

    let mut packages = Vec::<Value>::new();
    let mut names = BTreeSet::new();
    for entry in fs::read_dir(&patch_root).expect("read patches directory") {
        let entry = entry.expect("read patch directory entry");
        if !entry.file_type().expect("read patch entry type").is_dir() {
            continue;
        }
        let path = entry.path().join("package.toml");
        if !path.is_file() {
            continue;
        }
        println!("cargo:rerun-if-changed={}", path.display());
        let text = fs::read_to_string(&path).expect("read package.toml");
        let value: toml::Value = toml::from_str(&text).expect("parse package.toml");
        let package = value
            .get("package")
            .expect("package.toml requires [package]");
        let name = package
            .get("name")
            .and_then(toml::Value::as_str)
            .expect("[package].name must be a string");
        assert_eq!(
            entry.file_name().to_string_lossy(),
            name,
            "package directory and package name differ"
        );
        assert!(
            names.insert(name.to_owned()),
            "duplicate package name {name}"
        );

        let version = package
            .get("version")
            .and_then(toml::Value::as_str)
            .expect("[package].version must be a string");
        assert!(valid_version(version), "invalid package version {version}");
        let kind = package
            .get("kind")
            .and_then(toml::Value::as_str)
            .expect("[package].kind must be a string");
        let category = package.get("category").and_then(toml::Value::as_str);
        if kind == "patch" {
            assert!(
                matches!(category, Some("display" | "midi" | "effects")),
                "{name}: [package].category must be display, midi, or effects"
            );
        }
        let author = package
            .get("author")
            .and_then(toml::Value::as_str)
            .unwrap_or_else(|| panic!("{name}: [package].author must be a GitHub username"));
        assert!(
            valid_github_username(author),
            "{name}: invalid GitHub username `{author}`"
        );
        let mut metadata = serde_json::to_value(package).expect("serialize package metadata");
        let effects = value.get("effects");
        validate_effect_metadata(name, effects);
        if let Some(effects) = effects {
            metadata
                .as_object_mut()
                .expect("package metadata must be an object")
                .insert(
                    "effects".into(),
                    serde_json::to_value(effects).expect("serialize effect metadata"),
                );
        }
        packages.push(metadata);
    }

    packages.sort_by(|left, right| {
        let order = |package: &Value| package["order"].as_u64().unwrap_or(u64::MAX);
        order(left)
            .cmp(&order(right))
            .then_with(|| left["name"].as_str().cmp(&right["name"].as_str()))
    });

    let mut effect_ids = BTreeSet::new();
    for package in &packages {
        if let Some(effects) = package.get("effects").and_then(Value::as_array) {
            for effect in effects {
                let id = effect["id"].as_str().expect("validated effect id");
                assert!(
                    effect_ids.insert(id),
                    "duplicate effect implementation id {id}"
                );
            }
        }
    }

    let by_name = packages
        .iter()
        .map(|package| (package["name"].as_str().unwrap(), package))
        .collect::<BTreeMap<_, _>>();
    for package in &packages {
        let name = package["name"].as_str().unwrap();
        for dependency in package["requires"]
            .as_array()
            .expect("requires must be an array")
        {
            let dependency_name = dependency["name"]
                .as_str()
                .expect("dependency name must be a string");
            let version = dependency["version"]
                .as_str()
                .expect("dependency version must be a string");
            assert!(
                valid_version(version),
                "{name}: invalid dependency version {version}"
            );
            let dependency = dependency_name;
            assert!(
                by_name.contains_key(dependency),
                "package {name} requires missing package {dependency}"
            );
            assert_eq!(
                by_name[dependency]["version"].as_str(),
                Some(version),
                "package {name} requires {dependency} version {version}"
            );
        }
        validate_dependencies(name, &by_name, &mut BTreeSet::new(), &mut BTreeSet::new());
        validate_models(package);
    }
    let file_names = packages
        .iter()
        .map(|package| {
            let name = package["name"].as_str().unwrap();
            composed_file_name(name, &by_name)
        })
        .collect::<Vec<_>>();
    for (package, file_name) in packages.iter_mut().zip(file_names) {
        package
            .as_object_mut()
            .expect("package metadata must be an object")
            .insert("file_name".into(), Value::String(file_name));
    }

    let mut generated = String::new();
    let patch_names = package_names(&packages, "kind", "patch", "firmware_models");
    let wasm_names = package_names(&packages, "kind", "patch", "wasm_models");
    emit_name_array(&mut generated, "PATCHES", &patch_names);
    emit_name_array(&mut generated, "WASM_PATCHES", &wasm_names);

    let out_dir = PathBuf::from(env::var_os("OUT_DIR").unwrap());
    let catalog_path = out_dir.join("patch_catalog.json");
    fs::write(
        &catalog_path,
        serde_json::to_vec_pretty(&json!({"packages": packages})).unwrap(),
    )
    .expect("write package catalog JSON");
    generated.push_str(&format!(
        "pub const PATCH_CATALOG_JSON: &str = include_str!({:?});\n",
        catalog_path
    ));
    fs::write(out_dir.join("generated_catalog.rs"), generated)
        .expect("write generated Rust package catalog");
}

fn valid_version(version: &str) -> bool {
    let mut parts = version.split('.');
    let Some(major) = parts.next() else {
        return false;
    };
    let Some(minor) = parts.next() else {
        return false;
    };
    let Some(patch) = parts.next() else {
        return false;
    };
    parts.next().is_none()
        && [major, minor, patch]
            .iter()
            .all(|part| !part.is_empty() && part.bytes().all(|byte| byte.is_ascii_digit()))
}

fn valid_github_username(username: &str) -> bool {
    let bytes = username.as_bytes();
    !bytes.is_empty()
        && bytes.len() <= 39
        && bytes[0].is_ascii_alphanumeric()
        && bytes[bytes.len() - 1].is_ascii_alphanumeric()
        && bytes
            .iter()
            .all(|byte| byte.is_ascii_alphanumeric() || *byte == b'-')
        && !username.contains("--")
}

fn package_names(
    packages: &[Value],
    kind_field: &str,
    kind: &str,
    supported_field: &str,
) -> Vec<String> {
    packages
        .iter()
        .filter(|package| {
            package[kind_field] == kind
                && package[supported_field]
                    .as_array()
                    .is_some_and(|models| !models.is_empty())
        })
        .map(|package| package["name"].as_str().unwrap().to_owned())
        .collect()
}

fn emit_name_array(generated: &mut String, name: &str, values: &[String]) {
    generated.push_str(&format!("pub const {name}: [&str; {}] = [", values.len()));
    for value in values {
        generated.push_str(&format!("{value:?},"));
    }
    generated.push_str("];\n");
}

fn validate_models(package: &Value) {
    let name = package["name"].as_str().unwrap();
    let list = |key: &str| {
        package[key]
            .as_array()
            .unwrap_or_else(|| panic!("{name}: {key} must be an array"))
    };
    let models = list("models")
        .iter()
        .map(|model| model.as_str().expect("model names must be strings"))
        .collect::<BTreeSet<_>>();
    for key in [
        "firmware_models",
        "sd_card_models",
        "wasm_models",
        "website_sd_models",
        "emulator_sd_models",
    ] {
        for model in list(key) {
            let model = model
                .as_str()
                .unwrap_or_else(|| panic!("{name}: {key} entries must be strings"));
            assert!(
                models.contains(model),
                "{name}: {key} must be a subset of models"
            );
            assert!(
                model == "L6" || model == "L6max",
                "{name}: unsupported model in {key}"
            );
        }
    }
    assert!(package["name"].is_string());
    assert!(package["title"].is_string());
    assert!(package["summary"].is_string());
    assert!(package["description"].is_string());
    assert!(package["order"].is_number());
    assert!(package["requires"].is_array());
    for key in ["website", "emulator_gui"] {
        assert!(package[key].is_boolean(), "{name}: {key} must be a boolean");
    }
    if package["website"] == true {
        assert!(package["website_order"].is_number());
        assert!(package["readme"].is_string());
    }
    if package["emulator_gui"] == true {
        assert!(package["emulator_order"].is_number());
    }
}

fn validate_effect_metadata(name: &str, effects: Option<&toml::Value>) {
    let Some(effects) = effects else {
        return;
    };
    let effects = effects
        .as_array()
        .unwrap_or_else(|| panic!("{name}: [[effects]] must be an array"));
    assert!(!effects.is_empty(), "{name}: [[effects]] cannot be empty");
    let mut effect_ids = BTreeSet::new();
    for effect in effects {
        let id = effect
            .get("id")
            .and_then(toml::Value::as_str)
            .unwrap_or_else(|| panic!("{name}: effect id must be a string"));
        assert!(
            !id.is_empty()
                && id
                    .bytes()
                    .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-'),
            "{name}: effect id must be a lowercase kebab-case identifier"
        );
        assert!(effect_ids.insert(id), "{name}: duplicate effect id {id}");
        let title = effect
            .get("title")
            .and_then(toml::Value::as_str)
            .unwrap_or_else(|| panic!("{name}: effect title must be a string"));
        assert!(
            !title.trim().is_empty(),
            "{name}: effect title cannot be empty"
        );
        for key in ["based_on", "learn_more"] {
            if let Some(link) = effect.get(key) {
                let link = link
                    .as_table()
                    .unwrap_or_else(|| panic!("{name}: effect {key} must be a table"));
                let title = link
                    .get("title")
                    .and_then(toml::Value::as_str)
                    .unwrap_or_else(|| panic!("{name}: effect {key}.title must be a string"));
                let url = link
                    .get("url")
                    .and_then(toml::Value::as_str)
                    .unwrap_or_else(|| panic!("{name}: effect {key}.url must be a string"));
                assert!(
                    !title.trim().is_empty(),
                    "{name}: effect {key}.title cannot be empty"
                );
                assert!(
                    url.starts_with("https://") && url.len() > "https://".len(),
                    "{name}: effect {key}.url must be an HTTPS URL"
                );
            }
        }
        let controls = effect
            .get("controls")
            .and_then(toml::Value::as_array)
            .cloned()
            .unwrap_or_default();
        let mut unique_controls = BTreeSet::<String>::new();
        for control in controls {
            let control = control
                .as_str()
                .unwrap_or_else(|| panic!("{name}: effect controls entries must be strings"));
            assert!(
                ["encoder-parameters", "tap-tempo"].contains(&control),
                "{name}: unsupported effect control mode {control}"
            );
            assert!(
                unique_controls.insert(control.to_owned()),
                "{name}: duplicate effect control mode {control}"
            );
        }
        let tap_action = effect
            .get("tap_action")
            .and_then(toml::Value::as_str)
            .unwrap_or("stock");
        assert!(
            ["stock", "toggle-editor", "tempo"].contains(&tap_action),
            "{name}: unsupported effect tap action {tap_action}"
        );
        assert!(
            tap_action != "toggle-editor" || unique_controls.contains("encoder-parameters"),
            "{name}: toggle-editor requires encoder-parameters"
        );
        assert!(
            tap_action != "tempo" || unique_controls.contains("tap-tempo"),
            "{name}: tempo tap action requires tap-tempo control mode"
        );
    }
}

fn validate_dependencies(
    name: &str,
    packages: &BTreeMap<&str, &Value>,
    active: &mut BTreeSet<String>,
    complete: &mut BTreeSet<String>,
) {
    if complete.contains(name) {
        return;
    }
    assert!(
        active.insert(name.to_owned()),
        "package dependency cycle at {name}"
    );
    for dependency in packages[name]["requires"].as_array().unwrap() {
        validate_dependencies(
            dependency["name"].as_str().unwrap(),
            packages,
            active,
            complete,
        );
    }
    active.remove(name);
    complete.insert(name.to_owned());
}

fn composed_file_name(name: &str, packages: &BTreeMap<&str, &Value>) -> String {
    fn visit(
        name: &str,
        packages: &BTreeMap<&str, &Value>,
        complete: &mut BTreeSet<String>,
        ordered: &mut Vec<String>,
    ) {
        if !complete.insert(name.to_owned()) {
            return;
        }
        for dependency in packages[name]["requires"].as_array().unwrap() {
            visit(
                dependency["name"].as_str().unwrap(),
                packages,
                complete,
                ordered,
            );
        }
        ordered.push(name.to_owned());
    }

    let mut ordered = Vec::new();
    let mut complete = BTreeSet::new();
    visit(name, packages, &mut complete, &mut ordered);
    let mut members = vec![name.to_owned()];
    members.extend(ordered.into_iter().filter(|member| member != name));
    let stems = members
        .iter()
        .map(|member| {
            let package = packages[member.as_str()];
            format!(
                "{}-{}",
                package["name"].as_str().unwrap(),
                package["version"].as_str().unwrap()
            )
        })
        .collect::<Vec<_>>();
    format!("{}.bin", stems.join("-"))
}
