//! What one build installs: the requested patches, the effect map, and every
//! package they need, checked against the model and the way they are
//! delivered.
use l6fw::Model;

use crate::catalog::{
    self, PackageKind, PatchPackage, effect_replacement_file_name, effect_replacement_packages,
    resolve_dependencies,
};

/// How patches reach the device.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Delivery {
    /// Applied to the firmware by `l6patch`.
    Firmware,
    /// Applied to the firmware by the browser patcher.
    Browser,
    /// One module on the SD card, loaded by the Patch loader.
    SdCard,
}

impl Delivery {
    fn models(self, package: &PatchPackage) -> &[Model] {
        match self {
            Delivery::Firmware => &package.firmware_models,
            Delivery::Browser => &package.wasm_models,
            Delivery::SdCard => &package.sd_card_models,
        }
    }

    fn describe(self) -> &'static str {
        match self {
            Delivery::Firmware => "firmware",
            Delivery::Browser => "in the browser patcher",
            Delivery::SdCard => "SD cards",
        }
    }
}

/// A validated selection. Every package in it is a patch the model supports
/// for the delivery.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Plan {
    pub model: Model,
    pub delivery: Delivery,
    /// The patches asked for, then the packages the effect map needs.
    pub requested: Vec<String>,
    /// `requested` and everything it requires, in package order with every
    /// dependency before the patches that need it.
    pub patches: Vec<String>,
    /// `(stock_slot, effect_id)` pairs. Only an SD card module replaces
    /// effects.
    pub effects: Vec<(String, String)>,
}

impl Plan {
    pub fn new(
        model: Model,
        delivery: Delivery,
        patches: &[String],
        effects: &[(String, String)],
    ) -> Result<Plan, String> {
        let catalog = crate::patch_catalog();
        if !effects.is_empty() && delivery != Delivery::SdCard {
            return Err("effect replacements are available only in an SD card module".into());
        }
        let mut requested = Vec::<String>::new();
        for name in patches
            .iter()
            .cloned()
            .chain(effect_replacement_packages(effects, catalog)?)
        {
            if !requested.contains(&name) {
                requested.push(name);
            }
        }
        if requested.is_empty() && delivery == Delivery::SdCard {
            return Err("an SD card module needs at least one patch".into());
        }
        let resolved = resolve_dependencies(catalog, &requested)?;
        for name in &resolved {
            let package = catalog::package(catalog, name)?;
            if package.kind != PackageKind::Patch {
                return Err(format!("`{name}` is not a patch"));
            }
            if !delivery.models(package).contains(&model) {
                return Err(format!(
                    "patch `{name}` is not available for {} {}",
                    model.label(),
                    delivery.describe()
                ));
            }
        }
        Ok(Plan {
            model,
            delivery,
            requested,
            patches: resolved,
            effects: effects.to_vec(),
        })
    }

    /// The requested patches, then the packages only they require.
    pub fn members(&self) -> Vec<&str> {
        let mut members = Vec::<&str>::new();
        for name in self.requested.iter().chain(&self.patches) {
            if !members.contains(&name.as_str()) {
                members.push(name);
            }
        }
        members
    }

    /// The versioned name of the SD card module this plan builds.
    pub fn file_name(&self) -> Result<String, String> {
        let catalog = crate::patch_catalog();
        if self.delivery != Delivery::SdCard {
            return Err("only an SD card module has a file name".into());
        }
        if self.effects.is_empty() {
            catalog::composed_file_name(catalog, &self.requested)
        } else {
            effect_replacement_file_name(catalog, &self.effects)
        }
    }
}
