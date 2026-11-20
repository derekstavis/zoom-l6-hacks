//! What each action needs to run: the models it supports, the firmware it
//! reads, and whether it must be the scenario's first step. Scenario
//! validation checks the model scope and position when a scenario loads;
//! [`check_firmware`] checks the booted firmware before the guest starts, so
//! an unmet requirement is reported before anything runs.
use crate::firmware::FirmwareIdentity;
use crate::scenario::{Action, Scenario};

/// The firmware an action can run on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Firmware {
    /// The supported stock MAIN image, or an image our patch tools built from
    /// it and reported. Every scenario needs at least this.
    Supported,
    /// Exactly the supported stock MAIN image: the action reads addresses or
    /// behavior a patch could change.
    Stock,
}

/// What an action needs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Requirement {
    /// Models the action supports; empty means every model.
    pub models: &'static [&'static str],
    pub firmware: Firmware,
    /// The action must be the scenario's first step.
    pub first: bool,
}

const ANY: Requirement = Requirement {
    models: &[],
    firmware: Firmware::Supported,
    first: false,
};
const L6: &[&str] = &["l6"];
const L6MAX: &[&str] = &["l6max"];

const fn on(models: &'static [&'static str]) -> Requirement {
    Requirement { models, ..ANY }
}
const fn stock_on(models: &'static [&'static str]) -> Requirement {
    Requirement {
        models,
        firmware: Firmware::Stock,
        first: false,
    }
}

/// The requirement of `action`.
pub fn of(action: &Action) -> Requirement {
    match action {
        Action::AudioCallbacks => Requirement {
            first: true,
            ..stock_on(&[])
        },
        Action::ExpectSettings { .. } | Action::ExpectRecorder => stock_on(L6MAX),
        Action::L6maxChannelSignalPwm | Action::L6maxTapHandler => stock_on(L6MAX),
        Action::L6LevelMeterPwm
        | Action::L6DinSweep
        | Action::PanelEvent { .. }
        | Action::L6MainGpioEvent { .. }
        | Action::L6DinEventDispatch { .. } => stock_on(L6),
        Action::L6UsbStartup => Requirement {
            first: true,
            ..stock_on(L6)
        },
        Action::L6DinUsbForwarding | Action::L6InputSweep => on(L6),
        Action::L6maxRecorderPlayback { .. }
        | Action::L6maxInputTiming
        | Action::L6maxPanelKeyMap
        | Action::L6maxDinMixerControl { .. }
        | Action::KnobDialog
        | Action::KnobDialogPopup
        | Action::ExpectMenu { .. } => on(L6MAX),
        _ => ANY,
    }
}

/// The models `step` applies to in `scenario`.
fn step_models<'a>(scenario: &'a Scenario, step: &'a crate::scenario::Step) -> &'a [String] {
    if step.models.is_empty() {
        &scenario.models
    } else {
        &step.models
    }
}

/// Check each step's model scope and position.
pub fn check_scope(scenario: &Scenario) -> Result<(), String> {
    for (index, step) in scenario.step.iter().enumerate() {
        let requirement = of(&step.action);
        let name = action_name(&step.action);
        if !requirement.models.is_empty()
            && step_models(scenario, step)
                .iter()
                .any(|model| !requirement.models.contains(&model.as_str()))
        {
            return Err(format!(
                "{name} supports only {}",
                requirement.models.join(", ")
            ));
        }
        if requirement.first && index != 0 {
            return Err(format!("{name} must be the scenario's first step"));
        }
    }
    Ok(())
}

/// Check that `firmware`, the image the case boots on `model`, satisfies every
/// step that applies to that model.
pub fn check_firmware(
    scenario: &Scenario,
    model: &str,
    firmware: &FirmwareIdentity,
) -> Result<(), String> {
    let mut needed = Firmware::Supported;
    let mut needed_by = None;
    for step in &scenario.step {
        let applies = step.models.is_empty() || step.models.iter().any(|m| m == model);
        if applies && of(&step.action).firmware == Firmware::Stock {
            needed = Firmware::Stock;
            needed_by.get_or_insert_with(|| action_name(&step.action));
        }
    }
    match (needed, firmware) {
        (_, FirmwareIdentity::Stock { .. }) => Ok(()),
        (Firmware::Supported, FirmwareIdentity::Built { .. }) => Ok(()),
        (Firmware::Stock, FirmwareIdentity::Built { sha256 }) => Err(format!(
            "{} reads the stock firmware, but MAIN is a patched build ({sha256})",
            needed_by.unwrap_or_default()
        )),
        (_, FirmwareIdentity::Unknown { sha256 }) => Err(format!(
            "MAIN {sha256} is neither the supported stock image nor a build our patch tools reported"
        )),
    }
}

/// The action's name as scenarios write it.
fn action_name(action: &Action) -> String {
    toml::Value::try_from(action)
        .ok()
        .and_then(|value| value.get("action")?.as_str().map(str::to_owned))
        .unwrap_or_else(|| format!("{action:?}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scenario(models: &str, actions: &[&str]) -> Scenario {
        let steps: String = actions
            .iter()
            .map(|action| format!("[[step]]\naction='{action}'\n"))
            .collect();
        Scenario::from_text(&format!(
            "version=1\nname='t'\nmodels={models}\nbackends=['headless']\n{steps}"
        ))
        .unwrap()
    }

    #[test]
    fn stock_actions_need_the_stock_image_and_others_accept_reported_builds() {
        let stock = FirmwareIdentity::Stock {
            model: l6fw::Model::L6,
        };
        let built = FirmwareIdentity::Built {
            sha256: "b".repeat(64),
        };
        let unknown = FirmwareIdentity::Unknown {
            sha256: "c".repeat(64),
        };
        let reads_stock = scenario("['l6']", &["l6-din-sweep"]);
        let ordinary = scenario("['l6']", &["restart"]);
        assert!(check_firmware(&reads_stock, "l6", &stock).is_ok());
        assert!(
            check_firmware(&reads_stock, "l6", &built)
                .unwrap_err()
                .contains("l6-din-sweep")
        );
        assert!(check_firmware(&ordinary, "l6", &built).is_ok());
        for scenario in [&reads_stock, &ordinary] {
            assert!(check_firmware(scenario, "l6", &unknown).is_err());
        }
    }

    #[test]
    fn scope_and_position_come_from_the_table() {
        let error = Scenario::from_text(
            "version=1\nname='t'\nmodels=['l6max']\n[[step]]\naction='l6-din-sweep'\n",
        )
        .unwrap_err();
        assert!(error.contains("l6-din-sweep supports only l6"), "{error}");
        let late = Scenario::from_text(
            "version=1\nname='t'\nmodels=['l6']\n[[step]]\naction='delay'\nguest_ms=1\n[[step]]\naction='l6-usb-startup'\n",
        )
        .unwrap_err();
        assert!(late.contains("first step"), "{late}");
    }
}
