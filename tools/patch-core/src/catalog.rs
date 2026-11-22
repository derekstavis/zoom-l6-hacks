//! The patch catalog: every package's `package.toml`, typed and validated.
//!
//! The build script includes this file to read and check the packages, and
//! the library includes it to use them, so both read the same types and name
//! files with the same code.
use l6fw::Model;
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

/// Stable identifiers for L6max MAIN 1.10 stock effect slots exposed to
/// build-time replacement. Names are also used in package metadata.
pub const STOCK_EFFECT_SLOTS: [&str; 2] = ["room", "spring"];
pub const EFFECT_REPLACEMENT_ROUTER_PACKAGE: &str = "efx-loader";

/// A `package.toml` file.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PackageFile {
    pub package: PatchPackage,
    #[serde(default)]
    pub effects: Vec<EffectPackage>,
}

/// One patch or loader package.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct PatchPackage {
    pub name: String,
    pub version: String,
    pub kind: PackageKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub category: Option<PatchCategory>,
    pub order: u32,
    pub title: String,
    /// A GitHub username.
    pub author: String,
    pub summary: String,
    pub description: String,
    /// Every model the package supports; the lists below are subsets.
    #[serde(with = "labels")]
    pub models: Vec<Model>,
    /// Models whose firmware `l6patch` can patch with it.
    #[serde(with = "labels")]
    pub firmware_models: Vec<Model>,
    /// Models that can load it from the SD card.
    #[serde(with = "labels")]
    pub sd_card_models: Vec<Model>,
    /// Models the browser patcher can patch with it.
    #[serde(with = "labels")]
    pub wasm_models: Vec<Model>,
    pub website: bool,
    pub website_order: u32,
    /// Models the website offers its SD module for.
    #[serde(with = "labels")]
    pub website_sd_models: Vec<Model>,
    pub emulator_gui: bool,
    pub emulator_order: u32,
    /// Models the emulator bundles its SD module for.
    #[serde(with = "labels")]
    pub emulator_sd_models: Vec<Model>,
    pub readme: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub demo_image: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request_command: Option<String>,
    pub requires: Vec<PatchDependency>,
    /// The SD module file name, from [`composed_file_name`]. Filled in by
    /// the build script.
    #[serde(default)]
    pub file_name: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub effects: Vec<EffectPackage>,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum PackageKind {
    Patch,
    Loader,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct PatchDependency {
    pub name: String,
    pub version: String,
}

/// User-facing category used to organize selectable patches.
#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum PatchCategory {
    Display,
    Midi,
    Effects,
}

/// Build-time registration metadata for a custom effect implementation.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct EffectPackage {
    /// A lowercase kebab-case identifier, unique across the catalog.
    pub id: String,
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub based_on: Option<EffectLink>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub learn_more: Option<EffectLink>,
    #[serde(default)]
    pub controls: Vec<EffectControl>,
    #[serde(default)]
    pub tap_action: TapAction,
}

/// Optional attribution or background link for an effect package.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct EffectLink {
    pub title: String,
    /// An HTTPS URL.
    pub url: String,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "kebab-case")]
pub enum EffectControl {
    EncoderParameters,
    TapTempo,
}

/// What tapping the effect's slot does.
#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum TapAction {
    #[default]
    Stock,
    /// Open or close the effect's parameter editor; needs
    /// [`EffectControl::EncoderParameters`].
    ToggleEditor,
    /// Tap a tempo; needs [`EffectControl::TapTempo`].
    Tempo,
}

impl TapAction {
    /// The value efx-loader's generated `tap_action` returns.
    pub fn code(self) -> u32 {
        match self {
            TapAction::Stock => 0,
            TapAction::ToggleEditor => 1,
            TapAction::Tempo => 2,
        }
    }
}

/// Model lists are written with their labels, `L6` and `L6max`.
mod labels {
    use l6fw::Model;
    use serde::{Deserialize, Deserializer, Serializer, de::Error};

    pub fn serialize<S: Serializer>(models: &[Model], serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_seq(models.iter().map(|model| model.label()))
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Vec<Model>, D::Error> {
        Vec::<String>::deserialize(deserializer)?
            .iter()
            .map(|label| {
                Model::from_label(label)
                    .ok_or_else(|| D::Error::custom(format!("unsupported model {label}")))
            })
            .collect()
    }
}

impl PackageFile {
    /// The package with its effects, before [`finish`] names its file.
    pub fn into_package(self) -> PatchPackage {
        PatchPackage {
            effects: self.effects,
            ..self.package
        }
    }
}

/// Sort the packages, check them and name each one's SD module file.
pub fn finish(mut packages: Vec<PatchPackage>) -> Result<Vec<PatchPackage>, String> {
    packages.sort_by(|left, right| (left.order, &left.name).cmp(&(right.order, &right.name)));
    validate(&packages)?;
    let names = packages
        .iter()
        .map(|package| composed_file_name(&packages, std::slice::from_ref(&package.name)))
        .collect::<Result<Vec<_>, _>>()?;
    for (package, file_name) in packages.iter_mut().zip(names) {
        package.file_name = file_name;
    }
    Ok(packages)
}

/// Every rule a catalog must follow beyond what its types enforce.
pub fn validate(packages: &[PatchPackage]) -> Result<(), String> {
    let mut names = BTreeSet::new();
    let mut effect_ids = BTreeSet::new();
    for package in packages {
        let name = &package.name;
        if !names.insert(name.as_str()) {
            return Err(format!("duplicate package name {name}"));
        }
        if !valid_version(&package.version) {
            return Err(format!("{name}: invalid version {}", package.version));
        }
        if package.kind == PackageKind::Patch && package.category.is_none() {
            return Err(format!("{name}: a patch needs a category"));
        }
        if !valid_github_username(&package.author) {
            return Err(format!(
                "{name}: invalid GitHub username `{}`",
                package.author
            ));
        }
        for (list, models) in [
            ("firmware_models", &package.firmware_models),
            ("sd_card_models", &package.sd_card_models),
            ("wasm_models", &package.wasm_models),
            ("website_sd_models", &package.website_sd_models),
            ("emulator_sd_models", &package.emulator_sd_models),
        ] {
            if models.iter().any(|model| !package.models.contains(model)) {
                return Err(format!("{name}: {list} must be a subset of models"));
            }
        }
        for effect in &package.effects {
            validate_effect(name, effect)?;
            if !effect_ids.insert(effect.id.as_str()) {
                return Err(format!("duplicate effect implementation id {}", effect.id));
            }
        }
    }
    for package in packages {
        for dependency in &package.requires {
            let required = packages
                .iter()
                .find(|other| other.name == dependency.name)
                .ok_or_else(|| {
                    format!(
                        "{} requires missing package {}",
                        package.name, dependency.name
                    )
                })?;
            if required.version != dependency.version {
                return Err(format!(
                    "{} requires {} version {}, but the catalog has {}",
                    package.name, dependency.name, dependency.version, required.version
                ));
            }
            if required.kind != package.kind {
                return Err(format!(
                    "{} requires {}, which is a different kind of package",
                    package.name, dependency.name
                ));
            }
        }
    }
    for package in packages {
        resolve_dependencies(packages, std::slice::from_ref(&package.name))?;
    }
    Ok(())
}

fn validate_effect(name: &str, effect: &EffectPackage) -> Result<(), String> {
    let id = &effect.id;
    if id.is_empty()
        || !id
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
    {
        return Err(format!(
            "{name}: effect id {id} must be lowercase kebab-case"
        ));
    }
    if effect.title.trim().is_empty() {
        return Err(format!("{name}: effect {id} needs a title"));
    }
    for (field, link) in [
        ("based_on", &effect.based_on),
        ("learn_more", &effect.learn_more),
    ] {
        if let Some(link) = link
            && (link.title.trim().is_empty()
                || !link.url.starts_with("https://")
                || link.url.len() == "https://".len())
        {
            return Err(format!(
                "{name}: effect {id} {field} needs a title and an HTTPS URL"
            ));
        }
    }
    if effect.controls.iter().collect::<BTreeSet<_>>().len() != effect.controls.len() {
        return Err(format!("{name}: effect {id} lists a control twice"));
    }
    let needs = match effect.tap_action {
        TapAction::Stock => None,
        TapAction::ToggleEditor => Some(EffectControl::EncoderParameters),
        TapAction::Tempo => Some(EffectControl::TapTempo),
    };
    if needs.is_some_and(|control| !effect.controls.contains(&control)) {
        return Err(format!(
            "{name}: effect {id}'s tap action needs {needs:?} controls"
        ));
    }
    Ok(())
}

fn valid_version(version: &str) -> bool {
    let parts = version.split('.').collect::<Vec<_>>();
    parts.len() == 3
        && parts
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

fn package<'a>(catalog: &'a [PatchPackage], name: &str) -> Result<&'a PatchPackage, String> {
    catalog
        .iter()
        .find(|package| package.name == name)
        .ok_or_else(|| format!("patch `{name}` has no patches/{name}/package.toml"))
}

/// The requested packages and everything they require, in package order
/// with every dependency before the packages that need it.
pub fn resolve_dependencies(
    catalog: &[PatchPackage],
    requested: &[String],
) -> Result<Vec<String>, String> {
    fn visit<'a>(
        catalog: &'a [PatchPackage],
        name: &str,
        active: &mut BTreeSet<String>,
        complete: &mut BTreeSet<&'a str>,
    ) -> Result<(), String> {
        let package = package(catalog, name)?;
        if complete.contains(name) {
            return Ok(());
        }
        if !active.insert(name.to_owned()) {
            return Err(format!("patch dependency cycle at `{name}`"));
        }
        for dependency in &package.requires {
            visit(catalog, &dependency.name, active, complete)?;
        }
        active.remove(name);
        complete.insert(&package.name);
        Ok(())
    }

    let mut complete = BTreeSet::new();
    for name in requested {
        visit(catalog, name, &mut BTreeSet::new(), &mut complete)?;
    }
    // The catalog is in package order, so the first package whose
    // dependencies are all placed is the next one.
    let mut result = Vec::<String>::with_capacity(complete.len());
    while result.len() < complete.len() {
        let next = catalog
            .iter()
            .find(|package| {
                complete.contains(package.name.as_str())
                    && !result.contains(&package.name)
                    && package
                        .requires
                        .iter()
                        .all(|dependency| result.contains(&dependency.name))
            })
            .ok_or("patch dependency cycle")?;
        result.push(next.name.clone());
    }
    Ok(result)
}

/// The versioned file name of a module composed from `requested`: requested
/// packages first, then the packages they require, in resolved order.
pub fn composed_file_name(
    catalog: &[PatchPackage],
    requested: &[String],
) -> Result<String, String> {
    let ordered = resolve_dependencies(catalog, requested)?;
    let mut members = Vec::<&PatchPackage>::new();
    for name in requested.iter().chain(&ordered) {
        let package = package(catalog, name)?;
        if !members.iter().any(|member| member.name == package.name) {
            members.push(package);
        }
    }
    if members.is_empty() {
        return Err("composed bundle needs at least one patch name".into());
    }
    Ok(format!("{}.bin", stems(&members)))
}

fn stems(members: &[&PatchPackage]) -> String {
    members
        .iter()
        .map(|package| format!("{}-{}", package.name, package.version))
        .collect::<Vec<_>>()
        .join("-")
}

/// Validate a slot-to-effect map against a package catalog and return the
/// distinct selected effect package names in first-assignment order, after
/// the efx-loader router.
pub fn effect_replacement_packages(
    replacements: &[(String, String)],
    catalog: &[PatchPackage],
) -> Result<Vec<String>, String> {
    let mut slots = BTreeSet::new();
    let mut packages = Vec::new();
    if !replacements.is_empty() {
        package(catalog, EFFECT_REPLACEMENT_ROUTER_PACKAGE).map_err(|_| {
            format!(
                "missing effect replacement router package `{EFFECT_REPLACEMENT_ROUTER_PACKAGE}`"
            )
        })?;
        packages.push(EFFECT_REPLACEMENT_ROUTER_PACKAGE.to_owned());
    }
    for (slot, effect_id) in replacements {
        if !STOCK_EFFECT_SLOTS.contains(&slot.as_str()) {
            return Err(format!("unknown stock effect slot `{slot}`"));
        }
        if !slots.insert(slot) {
            return Err(format!(
                "stock effect slot `{slot}` has multiple replacements"
            ));
        }
        let package = catalog
            .iter()
            .find(|package| package.effects.iter().any(|effect| effect.id == *effect_id))
            .ok_or_else(|| format!("unknown effect implementation `{effect_id}`"))?;
        if !packages.contains(&package.name) {
            packages.push(package.name.clone());
        }
    }
    Ok(packages)
}

/// The versioned file name of a composed effect-replacement map. Every
/// member, the efx-loader router included, is named with its version, so a
/// change to any of them gives a new file. Stock slots are written
/// explicitly so each supported mapping has a unique SD asset name.
pub fn effect_replacement_file_name(
    catalog: &[PatchPackage],
    replacements: &[(String, String)],
) -> Result<String, String> {
    let requested = effect_replacement_packages(replacements, catalog)?;
    if requested.is_empty() {
        return Err("effect replacement bundle needs at least one assigned effect".into());
    }
    let effects = resolve_dependencies(catalog, &requested)?
        .iter()
        .filter(|name| name.as_str() != EFFECT_REPLACEMENT_ROUTER_PACKAGE)
        .map(|name| package(catalog, name))
        .collect::<Result<Vec<_>, _>>()?;
    let router = package(catalog, EFFECT_REPLACEMENT_ROUTER_PACKAGE)?;
    let assignment = |slot: &str| {
        replacements
            .iter()
            .find(|(selected, _)| selected == slot)
            .map_or("stock", |(_, id)| id.as_str())
    };
    Ok(format!(
        "{}-{}-room-{}-spring-{}.bin",
        stems(&[router]),
        stems(&effects),
        assignment("room"),
        assignment("spring")
    ))
}

/// Every assignment of the catalog's effects to the stock slots that
/// replaces at least one slot, room-major, with effects by id.
pub fn effect_assignments(catalog: &[PatchPackage]) -> Vec<Vec<(String, String)>> {
    let mut effects = catalog
        .iter()
        .flat_map(|package| package.effects.iter().map(|effect| effect.id.as_str()))
        .collect::<Vec<_>>();
    effects.sort();
    let choices = std::iter::once(None)
        .chain(effects.into_iter().map(Some))
        .collect::<Vec<_>>();
    let mut assignments = Vec::new();
    for room in &choices {
        for spring in &choices {
            let assignment = STOCK_EFFECT_SLOTS
                .into_iter()
                .zip([room, spring])
                .filter_map(|(slot, effect)| effect.map(|id| (slot.to_owned(), id.to_owned())))
                .collect::<Vec<_>>();
            if !assignment.is_empty() {
                assignments.push(assignment);
            }
        }
    }
    assignments
}
