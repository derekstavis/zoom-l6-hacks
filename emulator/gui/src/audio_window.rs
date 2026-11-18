//! The window that assigns WAV files to input channels and controls their playback.
use crate::NativePanel;
use gpui::{Context, Entity, Render, Subscription, Window, div, prelude::*, px};
use gpui_component::{
    ActiveTheme, Disableable, Sizable,
    alert::Alert,
    button::{Button, ButtonVariants},
    progress::ProgressCircle,
    slider::{Slider, SliderEvent, SliderState, SliderValue},
};
use l6max_host::{controls::audio_input_channels, protocol};
use std::time::Duration;

pub const WIDTH: f32 = 560.0;
const PADDING: f32 = 16.0;
const BAR_WIDTH: f32 = WIDTH - 2.0 * PADDING;

pub struct AudioWindow {
    panel: Entity<NativePanel>,
    seek_slider: Entity<SliderState>,
    seeking: bool,
    seek_target_ms: Option<u32>,
    _slider_subscription: Subscription,
}

impl AudioWindow {
    pub fn new(panel: Entity<NativePanel>, cx: &mut Context<Self>) -> Self {
        let seek_slider = cx.new(|_| SliderState::new());
        let panel_for_seek = panel.clone();
        let slider_subscription =
            cx.subscribe(&seek_slider, move |this, _, event, cx| match event {
                SliderEvent::Change(_) => this.seeking = true,
                SliderEvent::Release(SliderValue::Single(percent)) => {
                    panel_for_seek.update(cx, |panel, cx| {
                        let position = panel.audio_length_ms as f32 * (*percent / 100.0);
                        panel.audio_input_seek(position as i32, cx);
                    });
                    this.seek_target_ms = Some(
                        (panel_for_seek.read(cx).audio_length_ms as f32 * (*percent / 100.0))
                            as u32,
                    );
                    this.seeking = false;
                    cx.notify();
                }
                SliderEvent::Release(SliderValue::Range(_, _)) => {
                    unreachable!("single-value slider")
                }
            });
        // Position and conversion progress change without any input to this window.
        cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor()
                    .timer(Duration::from_millis(100))
                    .await;
                if this.update(cx, |_, cx| cx.notify()).is_err() {
                    break;
                }
            }
        })
        .detach();
        Self {
            panel,
            seek_slider,
            seeking: false,
            seek_target_ms: None,
            _slider_subscription: slider_subscription,
        }
    }
}

fn button(id: impl Into<gpui::ElementId>, label: &str, enabled: bool) -> Button {
    Button::new(id).label(label).secondary().disabled(!enabled)
}

fn time(milliseconds: u32) -> String {
    let seconds = milliseconds / 1000;
    format!("{}:{:02}", seconds / 60, seconds % 60)
}

fn audio_channel_row(
    channel: u32,
    mono_channels: u32,
    name: String,
    has_file: bool,
    converted: Option<f32>,
    converting: bool,
    panel: Entity<NativePanel>,
    cx: &mut Context<AudioWindow>,
) -> gpui::AnyElement {
    let channel_label = div()
        .w(px(20.0))
        .child(channel.to_string())
        .into_any_element();
    let kind_label = div()
        .w(px(52.0))
        .text_color(cx.theme().muted_foreground)
        .child(if channel <= mono_channels {
            "Mono"
        } else {
            "Stereo"
        })
        .into_any_element();
    let file_name = div()
        .flex_1()
        .min_w_0()
        .overflow_hidden()
        .whitespace_nowrap()
        .text_color(if has_file {
            cx.theme().foreground
        } else {
            cx.theme().muted_foreground
        })
        .child(name);
    let progress = converted.map(|converted| {
        ProgressCircle::new(format!("conversion-{channel}"))
            .with_size(gpui_component::Size::Small)
            .value(converted.clamp(0.0, 1.0) * 100.0)
            .accessibility_label(format!("Converting audio input channel {channel}"))
    });
    let file_info = div()
        .flex_1()
        .min_w_0()
        .flex()
        .items_center()
        .gap_2()
        .child(file_name)
        .when_some(progress, |element, progress| element.child(progress))
        .into_any_element();
    let choose_panel = panel.clone();
    let choose = button(("choose", channel), "Choose…", !converting)
        .when(!converting, |element| {
            element.on_click(move |_, _, cx| {
                choose_panel.update(cx, |panel, cx| panel.choose_audio_file(channel, cx));
            })
        })
        .into_any_element();
    let clear_panel = panel;
    let clear = button(("clear", channel), "Clear", !converting && has_file)
        .when(!converting && has_file, |element| {
            element.on_click(move |_, _, cx| {
                clear_panel.update(cx, |panel, cx| panel.set_audio_file(channel, None, cx));
            })
        })
        .into_any_element();
    div()
        .flex()
        .items_center()
        .gap_2()
        .child(channel_label)
        .child(kind_label)
        .child(file_info)
        .child(choose)
        .child(clear)
        .into_any_element()
}

fn audio_play(panel: &mut NativePanel, cx: &mut Context<NativePanel>) {
    panel.audio_input_transport(protocol::AUDIO_INPUT_PLAY, cx);
}

fn audio_pause(panel: &mut NativePanel, cx: &mut Context<NativePanel>) {
    panel.audio_input_transport(protocol::AUDIO_INPUT_PAUSE, cx);
}

fn audio_stop(panel: &mut NativePanel, cx: &mut Context<NativePanel>) {
    panel.audio_input_transport(protocol::AUDIO_INPUT_STOP, cx);
}

fn audio_rewind(panel: &mut NativePanel, cx: &mut Context<NativePanel>) {
    panel.audio_input_seek(0, cx);
}

fn panel_action_button(
    id: &'static str,
    label: &str,
    enabled: bool,
    panel: Entity<NativePanel>,
    action: fn(&mut NativePanel, &mut Context<NativePanel>),
) -> gpui::AnyElement {
    button(id, label, enabled)
        .when(enabled, |element| {
            element.on_click(move |_, _, cx| panel.update(cx, action))
        })
        .into_any_element()
}

fn transport_button(
    id: &'static str,
    label: &str,
    enabled: bool,
    panel: Entity<NativePanel>,
    audio_window: Entity<AudioWindow>,
    action: fn(&mut NativePanel, &mut Context<NativePanel>),
) -> gpui::AnyElement {
    button(id, label, enabled)
        .when(enabled, |element| {
            element.on_click(move |_, _, cx| {
                audio_window.update(cx, |window, cx| {
                    window.seeking = false;
                    window.seek_target_ms = None;
                    cx.notify();
                });
                panel.update(cx, action);
            })
        })
        .into_any_element()
}

fn playback_controls(
    panel: Entity<NativePanel>,
    audio_window: Entity<AudioWindow>,
    playing: bool,
    can_play: bool,
    position_ms: u32,
    length_ms: u32,
    cx: &mut Context<AudioWindow>,
) -> gpui::AnyElement {
    let mut controls = vec![
        transport_button(
            "play",
            if playing { "Pause" } else { "Play" },
            can_play,
            panel.clone(),
            audio_window.clone(),
            if playing { audio_pause } else { audio_play },
        ),
        transport_button(
            "stop",
            "Stop",
            can_play,
            panel.clone(),
            audio_window.clone(),
            audio_stop,
        ),
        transport_button(
            "rewind",
            "Rewind",
            can_play,
            panel,
            audio_window,
            audio_rewind,
        ),
    ];
    controls.push(
        div()
            .flex_1()
            .text_right()
            .text_color(cx.theme().muted_foreground)
            .child(format!("{} / {}", time(position_ms), time(length_ms)))
            .into_any_element(),
    );
    div()
        .flex()
        .items_center()
        .gap_2()
        .children(controls)
        .into_any_element()
}

impl AudioWindow {
    fn render_element(&mut self, window: &mut Window, cx: &mut Context<Self>) -> gpui::AnyElement {
        let panel = self.panel.read(cx);
        let model = panel.model;
        let progress = panel.audio_progress();
        let converting = panel.audio_conversion.is_some();
        let patch = panel.audio_patch.clone();
        let error = panel.audio_error.clone();
        let length_ms = panel.audio_length_ms;
        let (reported_position_ms, playing) = panel.audio_position();
        let position_ms = if let Some(target_ms) = self.seek_target_ms {
            if reported_position_ms.abs_diff(target_ms) <= 250 {
                self.seek_target_ms = None;
                reported_position_ms
            } else {
                target_ms
            }
        } else {
            reported_position_ms
        };
        let running = panel.controls.is_some();
        let channels = audio_input_channels(model);
        let mono_channels = match model {
            l6fw::Model::L6 => 2,
            l6fw::Model::L6max => 4,
        };
        let rows = (1..=channels)
            .map(|channel| {
                let file = patch.get(&channel);
                let name = file.and_then(|path| path.file_name()).map_or_else(
                    || "No file".to_owned(),
                    |name| name.to_string_lossy().into_owned(),
                );
                audio_channel_row(
                    channel,
                    mono_channels,
                    name,
                    file.is_some(),
                    progress.get(&channel).copied(),
                    converting,
                    self.panel.clone(),
                    cx,
                )
            })
            .collect::<Vec<_>>();
        let can_play = running && length_ms > 0 && !converting;
        let percent = if length_ms > 0 {
            position_ms as f32 / length_ms as f32 * 100.0
        } else {
            0.0
        };
        if !self.seeking {
            self.seek_slider.update(cx, |slider, cx| {
                if (slider.value().end() - percent).abs() > 0.5 {
                    slider.set_value(percent, window, cx);
                }
            });
        }
        let mut children = rows;
        children.push(
            div()
                .flex()
                .gap_2()
                .pt_1()
                .child(panel_action_button(
                    "assign",
                    "Assign Files…",
                    !converting,
                    self.panel.clone(),
                    NativePanel::assign_audio_inputs,
                ))
                .child(panel_action_button(
                    "clear-all",
                    "Clear All",
                    !converting && !patch.is_empty(),
                    self.panel.clone(),
                    NativePanel::clear_audio_inputs,
                ))
                .into_any_element(),
        );
        if let Some(error) = error {
            children.push(Alert::error("audio-error", error).into_any_element());
        }
        children.push(
            Slider::new(&self.seek_slider)
                .disabled(!can_play)
                .accessibility_label("Playback position")
                .w(px(BAR_WIDTH))
                .pt_2()
                .into_any_element(),
        );
        children.push(playback_controls(
            self.panel.clone(),
            cx.entity().clone(),
            playing,
            can_play,
            position_ms,
            length_ms,
            cx,
        ));
        div()
            .size_full()
            .p(px(PADDING))
            .bg(cx.theme().background)
            .text_color(cx.theme().foreground)
            .text_sm()
            .flex()
            .flex_col()
            .gap_2()
            .children(children)
            .into_any_element()
    }
}

impl Render for AudioWindow {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.render_element(window, cx)
    }
}
