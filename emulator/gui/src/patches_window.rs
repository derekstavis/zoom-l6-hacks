//! The window that selects which patches the guest runs, and whether each is
//! installed in the firmware or loaded from the SD card by the Patch loader.
use crate::NativePanel;
use gpui::{Context, CursorStyle, Entity, MouseButton, Render, Window, div, prelude::*, px};
use gpui_component::{
    ActiveTheme, Disableable,
    alert::Alert,
    button::{Button, ButtonVariants, Toggle, ToggleGroup, ToggleVariants},
    checkbox::Checkbox,
    group_box::{GroupBox, GroupBoxVariants},
    popover::Popover,
    scroll::ScrollableElement,
    switch::Switch,
    tab::{Tab as KitTab, TabBar},
};
use patch_core::{EffectPackage, PATCHES, PatchCategory, PatchPackage, patch_catalog};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::PathBuf,
};

pub const WIDTH: f32 = 520.0;
pub const HEIGHT: f32 = 700.0;
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
        .filter(|package| package.models.contains(&l6fw::Model::L6max))
        .flat_map(|package| package.effects.iter().map(move |effect| (package, effect)))
        .collect()
}

fn effect_ids(selection: &Selection) -> BTreeSet<String> {
    selection.effect_replacements.values().cloned().collect()
}

fn required_patches(
    selection: &Selection,
    effects: &BTreeSet<String>,
) -> BTreeMap<String, Delivery> {
    fn include(name: &str, delivery: Delivery, required: &mut BTreeMap<String, Delivery>) {
        if let Some(package) = patch_core::patch_package(name) {
            for dependency in &package.requires {
                if required.insert(dependency.name.clone(), delivery).is_none() {
                    include(&dependency.name, delivery, required);
                }
            }
        }
    }
    let mut required = BTreeMap::new();
    for name in &selection.firmware {
        include(name, Delivery::Firmware, &mut required);
    }
    for name in &selection.sd_card {
        include(name, Delivery::SdCard, &mut required);
    }
    for (package, effect) in offered_effects() {
        if effects.contains(&effect.id) {
            include(&package.name, Delivery::SdCard, &mut required);
        }
    }
    required
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
    category: PatchCategory,
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
            category: PatchCategory::Display,
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
            category: PatchCategory::Display,
            source: Source::Fixed(status),
        }
    }

    #[cfg(feature = "headless-screenshots")]
    #[allow(dead_code)]
    pub fn select_category_for_screenshot(&mut self, category: &str) {
        self.category = match category {
            "display" => PatchCategory::Display,
            "midi" => PatchCategory::Midi,
            "effects" => PatchCategory::Effects,
            _ => panic!("unknown patch category: {category}"),
        };
    }

    #[cfg(feature = "headless-screenshots")]
    #[allow(dead_code)]
    pub fn open_effect_menu_for_screenshot(&mut self, effect: &str) {
        self.open_effect_menu = Some(effect.to_owned());
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

fn effect_title(
    package: &'static PatchPackage,
    effect: &'static EffectPackage,
    checked: bool,
    cx: &mut Context<PatchesWindow>,
) -> gpui::AnyElement {
    let id = effect.id.as_str();
    let title_toggle_id = id.to_owned();
    let switch = Switch::new(format!("effect-{id}-toggle"))
        .checked(checked)
        .accessibility_label(format!("Enable {}", effect.title))
        .on_change(cx.listener(move |window, value, _, cx| {
            window.set_effect_enabled(id, *value);
            cx.notify();
        }));
    let title = div()
        .id(gpui::SharedString::from(format!("effect-{id}-title")))
        .flex()
        .flex_1()
        .min_w_0()
        .flex_col()
        .cursor(CursorStyle::PointingHand)
        .on_mouse_down(
            MouseButton::Left,
            cx.listener(move |window, _, _, cx| {
                window.set_effect_enabled(&title_toggle_id, !checked);
                cx.notify();
            }),
        )
        .child(
            div()
                .text_color(cx.theme().foreground)
                .child(effect.title.as_str())
                .into_any_element(),
        )
        .child(
            div()
                .text_xs()
                .text_color(cx.theme().muted_foreground)
                .child(format!("v{} · by {}", package.version, package.author))
                .into_any_element(),
        )
        .into_any_element();
    div()
        .flex()
        .items_center()
        .justify_between()
        .gap_3()
        .child(title)
        .child(switch.into_any_element())
        .into_any_element()
}

fn effect_installation(id: &str, cx: &mut Context<PatchesWindow>) -> gpui::AnyElement {
    let label = div()
        .text_xs()
        .text_color(cx.theme().muted_foreground)
        .child("Installation method")
        .into_any_element();
    let toggle = ToggleGroup::new(format!("effect-{id}-delivery"))
        .single()
        .segmented()
        .outline()
        .disabled(true)
        .child(
            Toggle::new(format!("effect-{id}-firmware"))
                .label("Firmware")
                .checked(false)
                .disabled(true),
        )
        .child(
            Toggle::new(format!("effect-{id}-sd-card"))
                .label("SD card")
                .checked(true)
                .disabled(true),
        )
        .into_any_element();
    div()
        .flex()
        .items_center()
        .justify_between()
        .gap_3()
        .child(label)
        .child(toggle)
        .into_any_element()
}

fn effect_slot_row(
    id: &'static str,
    slot: &'static str,
    selection: &Selection,
    has_card: bool,
    cx: &mut Context<PatchesWindow>,
) -> gpui::AnyElement {
    let current = selection.effect_replacements.get(slot);
    let selected = current.is_some_and(|assigned| assigned == id);
    let occupied_by = current.filter(|assigned| assigned.as_str() != id);
    let checkbox = Checkbox::new(format!("effect-{id}-slot-{slot}"))
        .label(slot_label(slot))
        .checked(selected)
        .disabled(!(has_card || selected))
        .on_change(cx.listener({
            let slot_id = slot.to_owned();
            let effect_id = id.to_owned();
            move |window, value, _, cx| {
                window
                    .selection
                    .set_effect(&slot_id, if *value { Some(&effect_id) } else { None });
                window.enabled_effects.insert(effect_id.clone());
                cx.notify();
            }
        }))
        .into_any_element();
    let note = occupied_by.map(|other| {
        let title = patch_core::patch_catalog()
            .iter()
            .flat_map(|package| package.effects.iter().map(move |effect| (package, effect)))
            .find(|(_, effect)| effect.id == *other)
            .map(|(_, effect)| effect.title.as_str())
            .unwrap_or(other.as_str());
        div()
            .text_xs()
            .text_color(cx.theme().muted_foreground)
            .child(format!("Used by {title}"))
            .into_any_element()
    });
    div()
        .flex()
        .items_center()
        .justify_between()
        .gap_3()
        .child(checkbox)
        .children(note)
        .into_any_element()
}

fn effect_assignment(
    id: &'static str,
    label: String,
    opened: bool,
    selection: &Selection,
    has_card: bool,
    cx: &mut Context<PatchesWindow>,
) -> gpui::AnyElement {
    let options = ["room", "spring"]
        .into_iter()
        .map(|slot| effect_slot_row(id, slot, selection, has_card, cx))
        .collect::<Vec<_>>();
    let assign_id = id.to_owned();
    let trigger = gpui_component::button::Button::new(format!("effect-{id}-assignment-trigger"))
        .secondary()
        .label(label);
    let popover = Popover::new(format!("effect-{id}-assignment"))
        .anchor(gpui::Anchor::BottomLeft)
        .trigger(trigger)
        .open(opened)
        .on_open_change(cx.listener(move |window, open, _, cx| {
            window.open_effect_menu = if *open { Some(assign_id.clone()) } else { None };
            cx.notify();
        }))
        .child(
            div()
                .w(px(220.0))
                .flex()
                .flex_col()
                .gap_1()
                .children(options)
                .into_any_element(),
        )
        .into_any_element();
    let label = div()
        .text_xs()
        .text_color(cx.theme().muted_foreground)
        .child("Assign to")
        .into_any_element();
    div()
        .flex()
        .items_center()
        .justify_between()
        .w_full()
        .child(label)
        .child(popover)
        .into_any_element()
}

fn effect_card(
    package: &'static PatchPackage,
    effect: &'static EffectPackage,
    selection: &Selection,
    enabled_effects: &BTreeSet<String>,
    open_effect_menu: Option<&str>,
    has_card: bool,
    cx: &mut Context<PatchesWindow>,
) -> gpui::AnyElement {
    let id = effect.id.as_str();
    let assigned_slots = ["room", "spring"]
        .into_iter()
        .filter(|slot| {
            selection
                .effect_replacements
                .get(*slot)
                .is_some_and(|assigned| assigned == id)
        })
        .collect::<Vec<_>>();
    let checked = enabled_effects.contains(id) || !assigned_slots.is_empty();
    let assignment_label = match assigned_slots.as_slice() {
        [] => "Choose slots".to_owned(),
        [slot] => slot_label(slot).to_owned(),
        _ => "Room, Spring".to_owned(),
    };
    let mut children = vec![effect_title(package, effect, checked, cx)];
    children.push(
        div()
            .text_sm()
            .text_color(cx.theme().muted_foreground)
            .child(package.description.as_str())
            .into_any_element(),
    );
    if !package.requires.is_empty() {
        children.push(dependency_label(package, cx));
    }
    children.push(effect_installation(id, cx));
    if checked {
        children.push(effect_assignment(
            id,
            assignment_label,
            open_effect_menu == Some(id),
            selection,
            has_card,
            cx,
        ));
    }
    GroupBox::new()
        .outline()
        .gap_2()
        .children(children)
        .into_any_element()
}

fn dependency_label(package: &PatchPackage, cx: &mut Context<PatchesWindow>) -> gpui::AnyElement {
    let dependencies = package
        .requires
        .iter()
        .map(|dependency| format!("{} v{}", dependency.name, dependency.version))
        .collect::<Vec<_>>()
        .join(", ");
    div()
        .text_xs()
        .text_color(cx.theme().muted_foreground)
        .child(format!("Requires: {dependencies}"))
        .into_any_element()
}

fn patch_header(
    package: &'static PatchPackage,
    patch: &'static str,
    checked: bool,
    togglable: bool,
    default_delivery: Delivery,
    cx: &mut Context<PatchesWindow>,
) -> gpui::AnyElement {
    let switch = Switch::new(format!("{patch}-toggle"))
        .checked(checked)
        .disabled(!togglable)
        .accessibility_label(format!("Enable {}", package.title))
        .on_change(cx.listener(move |window, value, _, cx| {
            window
                .selection
                .set(patch, if *value { Some(default_delivery) } else { None });
            cx.notify();
        }))
        .into_any_element();
    let title = div()
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
                .text_color(if togglable {
                    cx.theme().foreground
                } else {
                    cx.theme().muted_foreground
                })
                .child(package.title.as_str())
                .into_any_element(),
        )
        .child(
            div()
                .text_xs()
                .text_color(cx.theme().muted_foreground)
                .child(format!("{patch} · v{}", package.version))
                .into_any_element(),
        )
        .into_any_element();
    div()
        .flex()
        .items_center()
        .justify_between()
        .gap_3()
        .child(title)
        .child(switch)
        .into_any_element()
}

fn patch_installation(
    patch: &'static str,
    delivery: Option<Delivery>,
    usable: bool,
    card_enabled: bool,
    required: bool,
    cx: &mut Context<PatchesWindow>,
) -> gpui::AnyElement {
    let method = ToggleGroup::new(format!("{patch}-delivery"))
        .single()
        .segmented()
        .outline()
        .child(
            Toggle::new(format!("{patch}-firmware"))
                .label("Firmware")
                .checked(delivery == Some(Delivery::Firmware))
                .disabled(!usable || required),
        )
        .child(
            Toggle::new(format!("{patch}-sd-card"))
                .label("SD card")
                .checked(delivery == Some(Delivery::SdCard))
                .disabled(!card_enabled || required),
        )
        .on_click(cx.listener(move |window, values: &Vec<bool>, _, cx| {
            if values.first() == Some(&true) {
                window.selection.set(patch, Some(Delivery::Firmware));
                cx.notify();
            } else if values.get(1) == Some(&true) {
                window.selection.set(patch, Some(Delivery::SdCard));
                cx.notify();
            }
        }))
        .into_any_element();
    let label = div()
        .text_xs()
        .text_color(cx.theme().muted_foreground)
        .child("Installation method")
        .into_any_element();
    div()
        .flex()
        .items_center()
        .justify_between()
        .gap_3()
        .child(label)
        .child(method)
        .into_any_element()
}

fn patch_card(
    package: &'static PatchPackage,
    selection: &Selection,
    available: &[&str],
    loadable: &[&str],
    has_card: bool,
    required: Option<Delivery>,
    cx: &mut Context<PatchesWindow>,
) -> gpui::AnyElement {
    let patch = package.name.as_str();
    let usable = available.contains(&patch);
    let delivery = required.or_else(|| selection.delivery(patch));
    let checked = delivery.is_some();
    let from_card = loadable.contains(&patch);
    let card_enabled = has_card || delivery == Some(Delivery::SdCard);
    let default_delivery = if usable {
        Delivery::Firmware
    } else {
        Delivery::SdCard
    };
    let togglable = required.is_none() && (usable || checked || (from_card && card_enabled));
    let mut children = vec![
        patch_header(package, patch, checked, togglable, default_delivery, cx),
        div()
            .text_sm()
            .text_color(cx.theme().muted_foreground)
            .child(package.summary.as_str())
            .into_any_element(),
    ];
    if required.is_some() {
        children.push(
            div()
                .text_xs()
                .text_color(cx.theme().muted_foreground)
                .child("Included with a selected patch or effect")
                .into_any_element(),
        );
    }
    if !package.requires.is_empty() {
        children.push(dependency_label(package, cx));
    }
    if from_card {
        children.push(patch_installation(
            patch,
            delivery,
            usable,
            card_enabled,
            required.is_some(),
            cx,
        ));
    }
    GroupBox::new()
        .outline()
        .gap_2()
        .children(children)
        .into_any_element()
}

impl PatchesWindow {
    fn render_element(&mut self, _: &mut Window, cx: &mut Context<Self>) -> gpui::AnyElement {
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
        let selected_category = self.category;
        let required = required_patches(&self.selection, &self.enabled_effects);
        let rows = offered()
            .into_iter()
            .filter(|package| package.category == Some(selected_category))
            .map(|package| {
                patch_card(
                    package,
                    &self.selection,
                    &available,
                    &loadable,
                    has_card,
                    required.get(&package.name).copied(),
                    cx,
                )
            })
            .collect::<Vec<_>>();
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
        let effect_rows =
            if model == l6fw::Model::L6max && selected_category == PatchCategory::Effects {
                offered_effects()
                    .into_iter()
                    .map(|(package, effect)| {
                        effect_card(
                            package,
                            effect,
                            &self.selection,
                            &self.enabled_effects,
                            self.open_effect_menu.as_deref(),
                            has_card,
                            cx,
                        )
                    })
                    .collect::<Vec<_>>()
            } else {
                Vec::new()
            };
        let category_index = match selected_category {
            PatchCategory::Display => 0,
            PatchCategory::Midi => 1,
            PatchCategory::Effects => 2,
        };
        let category_tabs = TabBar::new("patch-category")
            .underline()
            .selected_index(category_index)
            .child(KitTab::new().label("Display"))
            .child(KitTab::new().label("MIDI"))
            .child(KitTab::new().label("Effects"))
            .on_click(cx.listener(|window, index: &usize, _, cx| {
                window.category = match index {
                    0 => PatchCategory::Display,
                    1 => PatchCategory::Midi,
                    _ => PatchCategory::Effects,
                };
                cx.notify();
            }));
        let message = patch_error.or_else(|| self.available.as_ref().err().cloned());
        div()
            .size_full()
            .flex()
            .flex_col()
            .gap_4()
            .p_4()
            .bg(cx.theme().background)
            .text_color(cx.theme().foreground)
            .child(category_tabs)
            .child(
                div()
                    .id("patch-list")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scrollbar()
                    .flex()
                    .flex_col()
                    .gap_3()
                    .children(effect_rows)
                    .children(rows),
            )
            .children(message.map(|message| Alert::error("patch-error", message)))
            .children(card_note.map(|note| {
                div()
                    .text_sm()
                    .text_color(cx.theme().muted_foreground)
                    .child(note)
            }))
            .child(
                div()
                    .text_sm()
                    .text_color(cx.theme().muted_foreground)
                    .child("Applying restarts the emulator. Settings are kept."),
            )
            .child(
                div().flex().justify_end().child(
                    Button::new("apply")
                        .label("Apply and Restart")
                        .primary()
                        .disabled(!enabled)
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.apply(this.selection.clone(), window, cx)
                        })),
                ),
            )
            .into_any_element()
    }
}

impl Render for PatchesWindow {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.render_element(window, cx)
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

    #[test]
    fn effects_require_their_dependency_on_sd_without_changing_the_selection() {
        let mut selection = Selection::new(names(&["knob-dialog"]), vec![]);
        let effects = ["velvet".to_owned(), "plateau".to_owned()]
            .into_iter()
            .collect();
        assert_eq!(
            required_patches(&selection, &effects).get("knob-dialog"),
            Some(&Delivery::SdCard)
        );
        selection.set_effect("room", Some("velvet"));
        assert_eq!(selection.delivery("knob-dialog"), Some(Delivery::Firmware));
        selection.set_effect("room", None);
        assert_eq!(selection.delivery("knob-dialog"), Some(Delivery::Firmware));
    }
}
