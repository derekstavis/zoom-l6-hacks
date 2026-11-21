//! JSON Schemas for case and fragment files, generated from the types that
//! parse them, for editor completion and validation (see `tombi.toml`).
//! Loading a scenario stays the authority: per-model symbol scope, step
//! position and file references are checked there, not here.
use crate::catalog::{Header, Suites};
use schemars::{JsonSchema, generate::SchemaSettings};
use serde_json::{Value, json};
use smoke_core::{Fragment, Scenario};
use std::{fs, path::Path};

/// Where the schemas live, from the repository root.
pub const DIR: &str = "tests/emulator/schemas";
pub const CASE: &str = "case.schema.json";
pub const FRAGMENT: &str = "fragment.schema.json";

/// A case file: a scenario with its `[case]` table, or a `[case]` table alone
/// for a case that runs another case's scenario or the packaged app.
pub fn case(suites: &Suites) -> Value {
    let mut generator = SchemaSettings::draft07().into_generator();
    let header = generator.subschema_for::<Header>();
    let mut schema = generator.into_root_schema_for::<Scenario>().to_value();
    close_steps(&mut schema);
    schema["title"] = json!("Emulator test case");
    let object = schema.as_object_mut().unwrap();
    let required = object.remove("required").unwrap_or_else(|| json!([]));
    object["properties"]["case"] = header.to_value();
    object.insert(
        "anyOf".into(),
        json!([
            { "required": required },
            { "required": ["case"], "maxProperties": 1 },
        ]),
    );
    let names: Vec<_> = suites.declared.keys().collect();
    schema["definitions"]["Header"]["properties"]["suites"]["items"] = json!({ "enum": names });
    schema
}

/// A fragment: steps that scenarios include.
pub fn fragment() -> Value {
    let mut schema = root::<Fragment>();
    close_steps(&mut schema);
    schema["title"] = json!("Emulator test fragment");
    schema
}

/// A step's own fields (`models`, timeouts) are generated beside its action
/// variants, which leaves every variant open to unknown keys. Give each
/// variant the step's fields and close it, so a misspelled key is an error.
fn close_steps(schema: &mut Value) {
    let step = &mut schema["definitions"]["Step"];
    let common = step.as_object_mut().unwrap().remove("properties").unwrap();
    for variant in step["oneOf"].as_array_mut().unwrap() {
        let properties = variant["properties"].as_object_mut().unwrap();
        for (key, value) in common.as_object().unwrap() {
            properties.insert(key.clone(), value.clone());
        }
        variant["additionalProperties"] = json!(false);
    }
    // The variants decide which keys a step allows; without this, a strict
    // validator also rejects every key at the step itself. Naming every
    // action here reports a misspelled one once, and completes it.
    let actions: Vec<_> = step["oneOf"]
        .as_array()
        .unwrap()
        .iter()
        .map(|variant| variant["properties"]["action"]["const"].clone())
        .collect();
    step["additionalProperties"] = json!(true);
    step["required"] = json!(["action"]);
    step["properties"] = json!({ "action": { "type": "string", "enum": actions } });
}

fn root<T: JsonSchema>() -> Value {
    SchemaSettings::draft07()
        .into_generator()
        .into_root_schema_for::<T>()
        .to_value()
}

/// The schema files and their contents.
pub fn files(root: &Path) -> Result<Vec<(std::path::PathBuf, String)>, String> {
    let suites = Suites::load(root)?;
    Ok([(CASE, case(&suites)), (FRAGMENT, fragment())]
        .into_iter()
        .map(|(name, schema)| {
            (
                root.join(DIR).join(name),
                serde_json::to_string_pretty(&sorted(schema)).unwrap() + "\n",
            )
        })
        .collect())
}

/// `value` with every object's keys in name order, so the files do not depend
/// on whether a build enables `serde_json/preserve_order`.
fn sorted(value: Value) -> Value {
    match value {
        Value::Object(object) => {
            let entries: std::collections::BTreeMap<_, _> = object
                .into_iter()
                .map(|(key, value)| (key, sorted(value)))
                .collect();
            Value::Object(entries.into_iter().collect())
        }
        Value::Array(items) => Value::Array(items.into_iter().map(sorted).collect()),
        other => other,
    }
}

/// Write the schemas.
pub fn write(root: &Path) -> Result<(), String> {
    fs::create_dir_all(root.join(DIR)).map_err(|e| e.to_string())?;
    for (path, text) in files(root)? {
        fs::write(&path, text).map_err(|e| format!("{}: {e}", path.display()))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    #[test]
    fn committed_schemas_match_the_scenario_types() {
        let root = l6max_host::paths::repository_root();
        for (path, expected) in super::files(root).unwrap() {
            let committed = std::fs::read_to_string(&path).unwrap_or_default();
            assert!(
                committed == expected,
                "{} is stale; run `cargo l6test schema`",
                path.display()
            );
        }
    }
}
