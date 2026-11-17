//! The window that selects which patches the guest runs, and whether each is
//! installed in the firmware or loaded from the SD card by the Patch loader.
use crate::NativePanel;
use firmware_patch_core::{EffectPackage, PATCHES, PatchPackage, patch_catalog};
use gpui::{Context, CursorStyle, Entity, MouseButton, Render, Window, div, prelude::*, px, rgb};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::PathBuf,
};

pub const WIDTH: f32 = 520.0;
pub const HEIGHT: f32 = 380.0;

fn offered() -> Vec<&'static PatchPackage> {
    let mut packages = patch_catalog()
        .iter()
        .filter(|package| package.emulator_gui)
        .collect::<Vec<_>>();
    packages.sort_by_key(|package| package.emulator_order);
    packages
}

fn offered_effects() -> Vec<(&'static PatchPackage, &'static EffectPackage)> {
    patch_catalog()
        .iter()
        .filter(|package| package.models.iter().any(|model| model == "L6max"))
        .flat_map(|package| package.effects.iter().map(move |effect| (package, effect)))
        .collect()
}

fn effect_ids(selection: &Selection) -> BTreeSet<String> {
    selection.effect_replacements.values().cloned().collect()
}

fn slot_label(slot: &str) -> &str {
    match slot {
        "room" => "Room",
        "spring" => "Spring",
        _ => slot,
    }
}

/// Where a selected patch comes from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Delivery {
    Firmware,
    SdCard,
}

/// The patches to run, split by where each comes from. A patch is in at most
/// one list.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Selection {
    pub firmware: Vec<String>,
    pub sd_card: Vec<String>,
    pub effect_replacements: BTreeMap<String, String>,
}

impl Selection {
    pub fn new(firmware: Vec<String>, sd_card: Vec<String>) -> Self {
        let mut selection = Self {
            firmware,
            sd_card,
            ..Self::default()
        };
        selection
            .firmware
            .retain(|patch| !selection.sd_card.contains(patch));
        selection.ordered()
    }

    pub fn with_effect_replacements(mut self, replacements: BTreeMap<String, String>) -> Self {
        self.effect_replacements = replacements;
        self
    }

    pub fn set_effect(&mut self, slot: &str, effect: Option<&str>) {
        match effect {
            Some(effect) => {
                self.effect_replacements
                    .insert(slot.to_owned(), effect.to_owned());
            }
            None => {
                self.effect_replacements.remove(slot);
            }
        }
    }

    pub fn delivery(&self, patch: &str) -> Option<Delivery> {
        if self.sd_card.iter().any(|name| name == patch) {
            Some(Delivery::SdCard)
        } else if self.firmware.iter().any(|name| name == patch) {
            Some(Delivery::Firmware)
        } else {
            None
        }
    }

    /// Select `patch` from `delivery`, or deselect it.
    pub fn set(&mut self, patch: &str, delivery: Option<Delivery>) {
        self.firmware.retain(|name| name != patch);
        self.sd_card.retain(|name| name != patch);
        match delivery {
            Some(Delivery::Firmware) => self.firmware.push(patch.to_owned()),
            Some(Delivery::SdCard) => self.sd_card.push(patch.to_owned()),
            None => {}
        }
        *self = std::mem::take(self).ordered();
    }

    /// Turn `patch` off, or on using its available installation method.
    pub fn toggle_with_default(&mut self, patch: &str, default: Delivery) {
        let next = self.delivery(patch).map_or(Some(default), |_| None);
        self.set(patch, next);
    }

    /// Catalog order, so that the same selection always reads and saves the same.
    fn ordered(self) -> Self {
        let order = |list: Vec<String>| -> Vec<String> {
            PATCHES
                .iter()
                .filter(|patch| list.iter().any(|name| name == *patch))
                .map(|patch| (*patch).to_owned())
                .collect()
        };
        Self {
            firmware: order(self.firmware),
            sd_card: order(self.sd_card),
            effect_replacements: self.effect_replacements,
        }
    }
}

/// What the window shows about the running emulator.
#[derive(Clone)]
pub struct Status {
    pub model: l6fw::Model,
    /// The firmware the other fields describe.
    pub firmware: PathBuf,
    pub can_start: bool,
    pub active: Selection,
    pub has_card: bool,
    pub patch_error: Option<String>,
}

impl Status {
    fn of(panel: &NativePanel) -> Self {
        let model = panel.model;
        Self {
            model,
            firmware: panel.options.firmware_dir.clone(),
            can_start: panel.options.qemu.is_some(),
            active: Selection::new(
                panel.options.selected_patches(model),
                panel.options.selected_sd_patches(model),
            )
            .with_effect_replacements(panel.options.selected_effect_replacements(model)),
            has_card: panel.options.sd_image.is_some(),
            patch_error: panel.patch_error.clone(),
        }
    }
}

enum Source {
    Panel(Entity<NativePanel>),
    /// A fixed status, for rendering without an emulator.
    #[cfg_attr(not(feature = "headless-screenshots"), allow(dead_code))]
    Fixed(Status),
}

pub struct PatchesWindow {
    source: Source,
    /// The firmware that `available` and `selection` describe.
    firmware: PathBuf,
    available: Result<Vec<&'static str>, String>,
    selection: Selection,
    enabled_effects: BTreeSet<String>,
    open_effect_menu: Option<String>,
}

/// A small two-way switch for a row's delivery.
fn segment(
    id: String,
    label: impl Into<gpui::SharedString>,
    selected: bool,
    bright: bool,
) -> gpui::Stateful<gpui::Div> {
    div()
        .id(gpui::SharedString::from(id))
        .px_2()
        .py(px(2.0))
        .text_sm()
        .bg(rgb(if selected { 0x2a3550 } else { 0x171d2c }))
        .text_color(rgb(match (selected, bright) {
            (true, _) => 0xe8eef8,
            (false, true) => 0x8b95aa,
            (false, false) => 0x4a5266,
        }))
        .child(label.into())
}

impl PatchesWindow {
    pub fn new(panel: Entity<NativePanel>, _: &mut Context<Self>) -> Self {
        Self {
            source: Source::Panel(panel),
            firmware: PathBuf::new(),
            available: Ok(Vec::new()),
            selection: Selection::default(),
            enabled_effects: BTreeSet::new(),
            open_effect_menu: None,
        }
    }

    /// A window showing `status`, with `selection` as the unapplied choice.
    #[cfg_attr(not(feature = "headless-screenshots"), allow(dead_code))]
    pub fn fixed(
        status: Status,
        available: Result<Vec<&'static str>, String>,
        selection: Selection,
    ) -> Self {
        Self {
            firmware: status.firmware.clone(),
            available,
            enabled_effects: effect_ids(&selection),
            selection,
            open_effect_menu: None,
            source: Source::Fixed(status),
        }
    }

    fn set_effect_enabled(&mut self, effect: &str, enabled: bool) {
        if enabled {
            self.enabled_effects.insert(effect.to_owned());
        } else {
            self.enabled_effects.remove(effect);
            self.selection
                .effect_replacements
                .retain(|_, assigned| assigned != effect);
        }
    }

    /// Close and restart with the selection. The main window shows the progress,
    /// and this window comes back if the selection cannot apply.
    pub fn apply(&mut self, selection: Selection, window: &mut Window, cx: &mut Context<Self>) {
        let selection = Selection::new(selection.firmware, selection.sd_card)
            .with_effect_replacements(selection.effect_replacements);
        if let Source::Panel(panel) = &self.source {
            panel.update(cx, |panel, cx| panel.apply_patches(selection, cx));
        }
        window.remove_window();
    }
}

impl Render for PatchesWindow {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let status = match &self.source {
            Source::Panel(panel) => {
                let panel = panel.read(cx);
                let status = Status::of(panel);
                // Follow a change of firmware or model while the window is open.
                if self.firmware != status.firmware {
                    self.firmware = status.firmware.clone();
                    self.available =
                        l6max_host::firmware_patches::available(&panel.options, status.model);
                    self.selection = status.active.clone();
                    self.enabled_effects = effect_ids(&self.selection);
                    self.open_effect_menu = None;
                }
                status
            }
            Source::Fixed(status) => status.clone(),
        };
        let Status {
            model,
            can_start,
            active,
            has_card,
            patch_error,
            ..
        } = status;
        let loadable = l6max_host::sd_patches::available(model);
        let available = self.available.as_deref().unwrap_or(&[]).to_vec();
        let enabled = self.selection != active && can_start;
        let rows = offered().into_iter().map(|package| {
            let patch = package.name.as_str();
            let usable = available.contains(&patch);
            let delivery = self.selection.delivery(patch);
            let checked = delivery.is_some();
            let from_card = loadable.contains(&patch);
            let card_enabled = has_card || delivery == Some(Delivery::SdCard);
            let default_delivery = if usable {
                Delivery::Firmware
            } else {
                Delivery::SdCard
            };
            // A saved patch can always be turned off, so that the guest can start again.
            let togglable = usable || checked || (from_card && card_enabled);
            let choose = |delivery: Delivery| {
                cx.listener(move |window: &mut Self, _, _, cx| {
                    window.selection.set(patch, Some(delivery));
                    cx.notify();
                })
            };
            let switch = div()
                .w(px(32.0))
                .h(px(18.0))
                .rounded_full()
                .bg(rgb(if checked { 0x5b8def } else { 0x34405c }))
                .flex()
                .items_center()
                .justify_start()
                .px(px(2.0))
                .child(
                    div()
                        .size(px(14.0))
                        .rounded_full()
                        .bg(rgb(0xe8eef8))
                        .when(checked, |knob| knob.ml(px(14.0))),
                );
            div()
                .flex()
                .flex_col()
                .gap_2()
                .rounded_md()
                .border_1()
                .border_color(rgb(0x34405c))
                .p_3()
                .child(
                    div()
                        .flex()
                        .items_center()
                        .justify_between()
                        .gap_3()
                        .child(
                            div()
                                .id(gpui::SharedString::from(format!("{patch}-title")))
                                .flex()
                                .flex_1()
                                .min_w_0()
                                .items_baseline()
                                .gap_2()
                                .when(togglable, |row| {
                                    row.cursor(CursorStyle::PointingHand).on_mouse_down(
                                        MouseButton::Left,
                                        cx.listener(move |window, _, _, cx| {
                                            window
                                                .selection
                                                .toggle_with_default(patch, default_delivery);
                                            cx.notify();
                                        }),
                                    )
                                })
                                .child(
                                    div()
                                        .min_w_0()
                                        .flex()
                                        .items_baseline()
                                        .gap_2()
                                        .child(
                                            div()
                                                .text_color(rgb(if togglable {
                                                    0xe8eef8
                                                } else {
                                                    0x6a7388
                                                }))
                                                .child(package.title.as_str()),
                                        )
                                        .child(
                                            div().text_xs().text_color(rgb(0x6a7388)).child(patch),
                                        ),
                                ),
                        )
                        .child(
                            div()
                                .id(gpui::SharedString::from(format!("{patch}-toggle")))
                                .when(togglable, |control| {
                                    control.cursor(CursorStyle::PointingHand).on_mouse_down(
                                        MouseButton::Left,
                                        cx.listener(move |window, _, _, cx| {
                                            window
                                                .selection
                                                .toggle_with_default(patch, default_delivery);
                                            cx.notify();
                                        }),
                                    )
                                })
                                .child(switch),
                        ),
                )
                .child(
                    div()
                        .text_sm()
                        .text_color(rgb(0x8b95aa))
                        .child(package.summary.as_str()),
                )
                .when(from_card, |card| {
                    let installation_method = div()
                        .flex()
                        .flex_none()
                        .rounded_md()
                        .border_1()
                        .border_color(rgb(0x34405c))
                        .overflow_hidden()
                        .child(
                            segment(
                                format!("{patch}-firmware"),
                                "Firmware",
                                delivery == Some(Delivery::Firmware),
                                usable && checked,
                            )
                            .when(usable, |segment| {
                                segment
                                    .cursor(CursorStyle::PointingHand)
                                    .on_mouse_down(MouseButton::Left, choose(Delivery::Firmware))
                            }),
                        )
                        .child(
                            segment(
                                format!("{patch}-sd-card"),
                                "SD card",
                                delivery == Some(Delivery::SdCard),
                                card_enabled && checked,
                            )
                            .when(card_enabled, |segment| {
                                segment
                                    .cursor(CursorStyle::PointingHand)
                                    .on_mouse_down(MouseButton::Left, choose(Delivery::SdCard))
                            }),
                        );
                    card.child(
                        div()
                            .flex()
                            .items_center()
                            .justify_between()
                            .gap_3()
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(rgb(0x8b95aa))
                                    .child("Installation method"),
                            )
                            .child(installation_method),
                    )
                })
        });
        let card_note: Option<&str> = if !has_card {
            Some("Insert an SD card (File → Use SD Card Folder…) to load patches from it.")
        } else if !self.selection.sd_card.is_empty()
            || !self.selection.effect_replacements.is_empty()
        {
            Some(
                "SD card patches add the Patch loader to the firmware and copy each patch \
                 to the card's PATCHES folder.",
            )
        } else {
            None
        };
        let effect_rows = if model == l6fw::Model::L6max {
            offered_effects()
                .into_iter()
                .map(|(package, effect)| {
                    let id = effect.id.as_str();
                    let assigned_slots = ["room", "spring"]
                        .into_iter()
                        .filter(|slot| {
                            self.selection
                                .effect_replacements
                                .get(*slot)
                                .is_some_and(|assigned| assigned == id)
                        })
                        .collect::<Vec<_>>();
                    let checked =
                        self.enabled_effects.contains(id) || !assigned_slots.is_empty();
                    let assignment_label = match assigned_slots.as_slice() {
                        [] => "Choose slots".to_owned(),
                        [slot] => slot_label(slot).to_owned(),
                        _ => "Room, Spring".to_owned(),
                    };
                    let opened = self.open_effect_menu.as_deref() == Some(id);
                    let toggle_id = id.to_owned();
                    let title_toggle_id = id.to_owned();
                    let toggle_enabled = checked;
                    let assign_id = id.to_owned();
                    div()
                        .flex()
                        .flex_col()
                        .gap_2()
                        .rounded_md()
                        .border_1()
                        .border_color(rgb(0x34405c))
                        .p_3()
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .justify_between()
                                .gap_3()
                                .child(
                                    div()
                                        .id(gpui::SharedString::from(format!("effect-{id}-title")))
                                        .flex()
                                        .flex_1()
                                        .min_w_0()
                                        .flex_col()
                                        .cursor(CursorStyle::PointingHand)
                                        .on_mouse_down(
                                            MouseButton::Left,
                                            cx.listener(move |window, _, _, cx| {
                                                window.set_effect_enabled(
                                                        &title_toggle_id,
                                                    !toggle_enabled,
                                                );
                                                cx.notify();
                                            }),
                                        )
                                        .child(
                                            div()
                                                .text_color(rgb(0xe8eef8))
                                                .child(effect.title.as_str()),
                                        )
                                        .child(
                                            div()
                                                .text_xs()
                                                .text_color(rgb(0x6a7388))
                                                .child(format!("by {}", package.author)),
                                        ),
                                )
                                .child(
                                    div()
                                        .id(gpui::SharedString::from(format!("effect-{id}-toggle")))
                                        .cursor(CursorStyle::PointingHand)
                                        .on_mouse_down(
                                            MouseButton::Left,
                                            cx.listener(move |window, _, _, cx| {
                                                window.set_effect_enabled(
                                                    &toggle_id,
                                                    !toggle_enabled,
                                                );
                                                cx.notify();
                                            }),
                                        )
                                        .child(
                                            div()
                                                .w(px(32.0))
                                                .h(px(18.0))
                                                .rounded_full()
                                                .bg(rgb(if checked { 0x5b8def } else { 0x34405c }))
                                                .flex()
                                                .items_center()
                                                .justify_start()
                                                .px(px(2.0))
                                                .child(
                                                    div()
                                                        .size(px(14.0))
                                                        .rounded_full()
                                                        .bg(rgb(0xe8eef8))
                                                        .when(checked, |knob| knob.ml(px(14.0))),
                                                ),
                                        ),
                                ),
                        )
                        .child(
                            div()
                                .text_sm()
                                .text_color(rgb(0x8b95aa))
                                .child(package.description.as_str()),
                        )
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .justify_between()
                                .gap_3()
                                .child(
                                    div()
                                        .text_xs()
                                        .text_color(rgb(0x8b95aa))
                                        .child("Installation method"),
                                )
                                .child(
                                    div()
                                        .flex()
                                        .flex_none()
                                        .rounded_md()
                                        .border_1()
                                        .border_color(rgb(0x34405c))
                                        .overflow_hidden()
                                        .child(segment(
                                            format!("effect-{id}-firmware"),
                                            "Firmware",
                                            false,
                                            false,
                                        ))
                                        .child(segment(
                                            format!("effect-{id}-sd-card"),
                                            "SD card",
                                            true,
                                            true,
                                        )),
                                ),
                        )
                        .when(checked, |card| {
                            card.child(
                                div()
                                    .flex()
                                    .flex_col()
                                    .gap_2()
                                    .child(
                                        div()
                                            .relative()
                                            .flex()
                                            .flex_col()
                                            .items_end()
                                            .gap_1()
                                            .child(
                                                div()
                                                    .flex()
                                                    .items_center()
                                                    .justify_between()
                                                    .w_full()
                                                    .child(
                                                        div()
                                                            .text_xs()
                                                            .text_color(rgb(0x8b95aa))
                                                            .child("Assign to"),
                                                    )
                                                    .child(
                                                        div()
                                                            .id(gpui::SharedString::from(format!("effect-{id}-assignment")))
                                                            .min_w(px(160.0))
                                                            .flex()
                                                            .items_center()
                                                            .justify_between()
                                                            .gap_3()
                                                            .rounded_md()
                                                            .border_1()
                                                            .border_color(rgb(0x34405c))
                                                            .bg(rgb(0x171d2c))
                                                            .px_2()
                                                            .py_1()
                                                            .text_sm()
                                                            .cursor(CursorStyle::PointingHand)
                                                            .on_mouse_down(
                                                                MouseButton::Left,
                                                                cx.listener(move |window, _, _, cx| {
                                                                    window.open_effect_menu =
                                                                        if window.open_effect_menu.as_deref()
                                                                            == Some(&assign_id)
                                                                        {
                                                                            None
                                                                        } else {
                                                                            Some(assign_id.clone())
                                                                        };
                                                                    cx.notify();
                                                                }),
                                                            )
                                                            .child(assignment_label)
                                                            .child(if opened { "▴" } else { "▾" }),
                                                    ),
                                            )
                                            .when(opened, |dropdown| {
                                                dropdown.child(
                                                    div()
                                                        .w(px(220.0))
                                                        .rounded_md()
                                                        .border_1()
                                                        .border_color(rgb(0x34405c))
                                                        .bg(rgb(0x171d2c))
                                                        .p_1()
                                                        .flex()
                                                        .flex_col()
                                                        .children(["room", "spring"].into_iter().map(
                                                            |slot| {
                                                                let current = self
                                                                    .selection
                                                                    .effect_replacements
                                                                    .get(slot);
                                                                let selected = current
                                                                    .is_some_and(|assigned| assigned == id);
                                                                let occupied_by = current
                                                                    .filter(|assigned| assigned.as_str() != id);
                                                                let can_change = has_card || selected;
                                                                let slot_id = slot.to_owned();
                                                                let effect_id = id.to_owned();
                                                                div()
                                                                    .id(gpui::SharedString::from(format!(
                                                                        "effect-{id}-slot-{slot}"
                                                                    )))
                                                                    .flex()
                                                                    .items_center()
                                                                    .gap_2()
                                                                    .rounded_sm()
                                                                    .px_2()
                                                                    .py_1()
                                                                    .text_sm()
                                                                    .text_color(rgb(if can_change {
                                                                        0xe8eef8
                                                                    } else {
                                                                        0x6a7388
                                                                    }))
                                                                    .when(can_change, |item| {
                                                                        item.cursor(
                                                                            CursorStyle::PointingHand,
                                                                        )
                                                                        .on_mouse_down(
                                                                            MouseButton::Left,
                                                                            cx.listener(
                                                                                move |window, _, _, cx| {
                                                                                    window.selection.set_effect(
                                                                                        &slot_id,
                                                                                        if selected {
                                                                                            None
                                                                                        } else {
                                                                                            Some(&effect_id)
                                                                                        },
                                                                                    );
                                                                                    window.enabled_effects.insert(
                                                                                        effect_id.clone(),
                                                                                    );
                                                                                    cx.notify();
                                                                                },
                                                                            ),
                                                                        )
                                                                    })
                                                                    .child(if selected { "✓" } else { "□" })
                                                                    .child(slot_label(slot))
                                                                    .when_some(occupied_by, |item, other| {
                                                                        let title = firmware_patch_core::patch_catalog()
                                                                            .iter()
                                                                            .flat_map(|package| {
                                                                                package.effects.iter().map(
                                                                                    move |effect| (package, effect),
                                                                                )
                                                                            })
                                                                            .find(|(_, effect)| effect.id == *other)
                                                                            .map(|(_, effect)| effect.title.as_str())
                                                                            .unwrap_or(other.as_str());
                                                                        item.child(
                                                                            div()
                                                                                .ml_auto()
                                                                                .text_xs()
                                                                                .text_color(rgb(0x6a7388))
                                                                                .child(format!("Used by {title}")),
                                                                        )
                                                                    })
                                                            },
                                                        )),
                                                )
                                            }),
                                    ),
                            )
                        })
                })
                .collect::<Vec<_>>()
        } else {
            Vec::new()
        };
        let message = patch_error.or_else(|| self.available.as_ref().err().cloned());
        div()
            .size_full()
            .flex()
            .flex_col()
            .gap_4()
            .p_4()
            .bg(rgb(0x10131c))
            .text_color(rgb(0xe8eef8))
            .child(
                div()
                    .id("patch-list")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .flex()
                    .flex_col()
                    .gap_3()
                    .when(model == l6fw::Model::L6max, |list| {
                        list.child(
                            div()
                                .text_sm()
                                .text_color(rgb(0xe8eef8))
                                .child("Effect patches"),
                        )
                    })
                    .children(effect_rows)
                    .children(rows),
            )
            .children(
                message.map(|message| div().text_sm().text_color(rgb(0xf0a0a0)).child(message)),
            )
            .children(card_note.map(|note| div().text_sm().text_color(rgb(0x8b95aa)).child(note)))
            .child(
                div()
                    .text_sm()
                    .text_color(rgb(0x8b95aa))
                    .child("Applying restarts the emulator. Settings are kept."),
            )
            .child(
                div().flex().justify_end().child(
                    div()
                        .id("apply")
                        .rounded_md()
                        .px_3()
                        .py_1()
                        .border_1()
                        .border_color(rgb(0x34405c))
                        .bg(rgb(if enabled { 0x2a3550 } else { 0x171d2c }))
                        .text_color(rgb(if enabled { 0xe8eef8 } else { 0x6a7388 }))
                        .child("Apply and Restart")
                        .when(enabled, |button| {
                            button.cursor(CursorStyle::PointingHand).on_mouse_down(
                                MouseButton::Left,
                                cx.listener(|this, _, window, cx| {
                                    this.apply(this.selection.clone(), window, cx)
                                }),
                            )
                        }),
                ),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(list: &[&str]) -> Vec<String> {
        list.iter().map(|name| (*name).to_owned()).collect()
    }

    #[test]
    fn every_offered_patch_has_a_title_and_description() {
        for package in offered() {
            assert!(
                !package.title.is_empty() && !package.summary.is_empty(),
                "{}",
                package.name
            );
        }
    }

    #[test]
    fn a_patch_has_one_delivery() {
        let mut selection = Selection::new(names(&["midi-sync"]), names(&["midi-sync"]));
        assert_eq!(selection.delivery("midi-sync"), Some(Delivery::SdCard));
        assert!(selection.firmware.is_empty());
        selection.set("midi-sync", Some(Delivery::Firmware));
        assert_eq!(selection, Selection::new(names(&["midi-sync"]), vec![]));
    }

    #[test]
    fn toggling_turns_on_from_firmware_and_off_from_either() {
        let mut selection = Selection::default();
        selection.toggle_with_default("knob-dialog", Delivery::Firmware);
        assert_eq!(selection.delivery("knob-dialog"), Some(Delivery::Firmware));
        selection.set("knob-dialog", Some(Delivery::SdCard));
        selection.toggle_with_default("knob-dialog", Delivery::Firmware);
        assert_eq!(selection, Selection::default());
    }

    #[test]
    fn effect_assignments_are_independent_and_stock_removes_a_slot() {
        let mut selection = Selection::default();
        selection.set_effect("room", Some("velvet"));
        selection.set_effect("spring", Some("plateau"));
        selection.set("midi-sync", Some(Delivery::SdCard));
        assert_eq!(selection.effect_replacements["room"], "velvet");
        assert_eq!(selection.effect_replacements["spring"], "plateau");
        selection.set_effect("room", None);
        assert!(!selection.effect_replacements.contains_key("room"));
        assert_eq!(selection.effect_replacements["spring"], "plateau");
    }

    #[test]
    fn choosing_a_delivery_turns_the_patch_on_in_catalog_order() {
        let mut selection = Selection::default();
        selection.set("midi-sync", Some(Delivery::SdCard));
        selection.set("knob-dialog", Some(Delivery::SdCard));
        selection.set("din-mixer-control", Some(Delivery::Firmware));
        assert_eq!(selection.sd_card, names(&["knob-dialog", "midi-sync"]));
        assert_eq!(selection.firmware, names(&["din-mixer-control"]));
    }
}
