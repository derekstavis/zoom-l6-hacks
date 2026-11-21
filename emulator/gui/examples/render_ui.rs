//! Render the patches, channels, and playback windows without compiling the
//! emulator's main view. For example:
//! `cargo run -p l6max-gui --example render_ui --features headless-screenshots -- l6max --window audio --output /tmp/audio.png`.
use gpui::{AnyWindowHandle, App, Context, HeadlessAppContext, WindowHandle, prelude::*, px, size};
use gpui_platform::{current_headless_renderer, current_platform};
use l6max_host::engine::Options;
use patches_window::{
    HEIGHT as PATCHES_HEIGHT, PatchesWindow, Selection, Status, WIDTH as PATCHES_WIDTH,
};
use std::{path::Path, sync::Arc};

#[allow(dead_code)]
#[path = "../src/audio_window.rs"]
mod audio_window;
#[allow(dead_code)]
#[path = "../src/components/mod.rs"]
mod components;
#[allow(dead_code)]
#[path = "../src/debug_window.rs"]
mod debug_window;
#[allow(dead_code)]
#[path = "../src/headless_screenshot.rs"]
mod headless_screenshot;
#[allow(dead_code)]
#[path = "../src/patches_window.rs"]
mod patches_window;

fn notice_anchor(model: l6fw::Model, scale: f32) -> gpui::Div {
    let plate = components::panel::nameplate(model);
    let (x, y) = (plate.x + plate.width / 2.0, plate.y + plate.height / 2.0);
    gpui::div()
        .absolute()
        .left(px(x * scale - 300.0))
        .top(px(y * scale - 30.0))
        .w(px(600.0))
        .h(px(60.0))
        .flex()
        .items_center()
        .justify_center()
}

fn notice_view(text: gpui::SharedString, error: bool) -> gpui::Div {
    gpui::div()
        .max_w(px(560.0))
        .px_4()
        .py_2()
        .rounded_lg()
        .border_1()
        .border_color(gpui::rgb(if error { 0x7a3a40 } else { 0x34405c }))
        .bg(gpui::rgb(if error { 0x3a1c22 } else { 0x171f30 }))
        .text_sm()
        .text_color(gpui::rgb(if error { 0xffb3a7 } else { 0xe8eef8 }))
        .child(text)
}

struct NativePanel {
    _qemu: Option<l6max_host::engine::Engine>,
    model: l6fw::Model,
    options: Options,
    patch_error: Option<String>,
    audio_patch: std::collections::BTreeMap<u32, std::path::PathBuf>,
    audio_conversion: Option<Arc<std::sync::Mutex<std::collections::BTreeMap<u32, f32>>>>,
    audio_error: Option<String>,
    audio_length_ms: u32,
    audio_position_ms: u32,
    audio_playing: bool,
    controls: Option<()>,
}

impl NativePanel {
    fn apply_patches(&mut self, _: Selection, _: &mut Context<Self>) {}

    fn audio_progress(&self) -> std::collections::BTreeMap<u32, f32> {
        self.audio_conversion
            .as_ref()
            .map_or_else(Default::default, |progress| {
                progress.lock().unwrap().clone()
            })
    }

    fn audio_position(&self) -> (u32, bool) {
        (self.audio_position_ms, self.audio_playing)
    }

    fn choose_audio_file(&mut self, _: u32, _: &mut Context<Self>) {}

    fn set_audio_file(
        &mut self,
        channel: u32,
        path: Option<std::path::PathBuf>,
        _: &mut Context<Self>,
    ) {
        if let Some(path) = path {
            self.audio_patch.insert(channel, path);
        } else {
            self.audio_patch.remove(&channel);
        }
    }

    fn assign_audio_inputs(&mut self, _: &mut Context<Self>) {}

    fn clear_audio_inputs(&mut self, _: &mut Context<Self>) {
        self.audio_patch.clear();
    }

    fn audio_input_seek(&mut self, position: i32, _: &mut Context<Self>) {
        self.audio_position_ms = position.max(0) as u32;
    }

    fn audio_input_transport(&mut self, command: u32, _: &mut Context<Self>) {
        match command {
            l6max_host::protocol::AUDIO_INPUT_PLAY => self.audio_playing = true,
            l6max_host::protocol::AUDIO_INPUT_PAUSE => self.audio_playing = false,
            l6max_host::protocol::AUDIO_INPUT_STOP => {
                self.audio_playing = false;
                self.audio_position_ms = 0;
            }
            _ => {}
        }
    }
}

fn option<'a>(args: &'a [String], name: &str) -> Option<&'a str> {
    args.windows(2)
        .find(|pair| pair[0] == name)
        .map(|pair| pair[1].as_str())
}

fn list(args: &[String], name: &str) -> Vec<String> {
    option(args, name)
        .map(|value| {
            value
                .split(',')
                .filter(|item| !item.is_empty())
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default()
}

fn effects(args: &[String], name: &str) -> std::collections::BTreeMap<String, String> {
    option(args, name)
        .into_iter()
        .flat_map(|value| value.split(','))
        .filter_map(|entry| {
            let (slot, effect) = entry.split_once('=')?;
            Some((slot.to_owned(), effect.to_owned()))
        })
        .collect()
}

fn open_audio_window(
    app: &mut HeadlessAppContext,
    model: l6fw::Model,
    height: f32,
) -> Result<AnyWindowHandle, Box<dyn std::error::Error>> {
    let options =
        Options::parse_args(["--no-auto-setup", "--no-sd"].map(std::ffi::OsString::from))?;
    let mut audio_patch = std::collections::BTreeMap::new();
    audio_patch.insert(1, "Kick.wav".into());
    audio_patch.insert(4, "Snare.wav".into());
    audio_patch.insert(7, "Room microphones.wav".into());
    let panel = NativePanel {
        _qemu: None,
        model,
        options,
        patch_error: None,
        audio_patch,
        audio_conversion: Some(Arc::new(std::sync::Mutex::new(
            [(1, 0.61), (4, 0.31)].into_iter().collect(),
        ))),
        audio_error: None,
        audio_length_ms: 61_000,
        audio_position_ms: 24_200,
        audio_playing: true,
        controls: Some(()),
    };
    let audio_window: WindowHandle<gpui_component::Root> = app.open_window(
        size(px(audio_window::WIDTH), px(height)),
        move |window, cx: &mut App| {
            components::theme::init(cx);
            let panel = cx.new(|_| panel);
            let audio = cx.new(|cx| audio_window::AudioWindow::new(panel, cx));
            cx.new(|cx| gpui_component::Root::new(audio, window, cx))
        },
    )?;
    Ok(audio_window.into())
}

fn open_debug_window(
    app: &mut HeadlessAppContext,
    model: l6fw::Model,
    kind: String,
    width: f32,
) -> Result<AnyWindowHandle, Box<dyn std::error::Error>> {
    let options =
        Options::parse_args(["--no-auto-setup", "--no-sd"].map(std::ffi::OsString::from))?;
    let panel = NativePanel {
        _qemu: None,
        model,
        options,
        patch_error: None,
        audio_patch: Default::default(),
        audio_conversion: None,
        audio_error: None,
        audio_length_ms: 0,
        audio_position_ms: 0,
        audio_playing: false,
        controls: None,
    };
    let handle: WindowHandle<gpui_component::Root> =
        app.open_window(size(px(width), px(600.)), move |window, cx: &mut App| {
            components::theme::init(cx);
            let panel = cx.new(|_| panel);
            let view = cx.new(|cx| debug_window::DebugWindow::preview(panel, &kind, window, cx));
            cx.new(|cx| gpui_component::Root::new(view, window, cx))
        })?;
    Ok(handle.into())
}

fn open_patches_window(
    app: &mut HeadlessAppContext,
    height: f32,
    args: &[String],
    status: Status,
    available: Result<Vec<&'static str>, String>,
    selection: Selection,
) -> Result<AnyWindowHandle, Box<dyn std::error::Error>> {
    let patches_window: WindowHandle<gpui_component::Root> = app.open_window(
        size(px(PATCHES_WIDTH), px(height)),
        |window, cx: &mut App| {
            components::theme::init(cx);
            let patches = cx.new(|_| {
                let mut window = PatchesWindow::fixed(status, available, selection);
                if let Some(category) = option(args, "--category") {
                    window.select_category_for_screenshot(category);
                }
                if let Some(effect) = option(args, "--open-menu") {
                    window.open_effect_menu_for_screenshot(effect);
                }
                window
            });
            cx.new(|cx| gpui_component::Root::new(patches, window, cx))
        },
    )?;
    Ok(patches_window.into())
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = std::env::args().skip(1).collect::<Vec<_>>();
    if option(&args, "--window") == Some("panel") {
        headless_screenshot::render(&args).map_err(std::io::Error::other)?;
        return Ok(());
    }
    let model = match args.first().map(String::as_str) {
        Some("l6") => l6fw::Model::L6,
        Some("l6max") => l6fw::Model::L6max,
        _ => return Err(
            "usage: render_patches <l6|l6max> [--effects room=plateau,spring=velvet] --output PATH"
                .into(),
        ),
    };
    let output = option(&args, "--output").ok_or("missing --output PATH")?;
    if Path::new(output).exists() {
        return Err(format!("refusing to overwrite {output}").into());
    }
    let selection = Selection::new(list(&args, "--firmware"), list(&args, "--sd-card"))
        .with_effect_replacements(effects(&args, "--effects"));
    let status = Status {
        model,
        firmware: "headless".into(),
        can_start: true,
        active: Selection::default(),
        has_card: !args.iter().any(|arg| arg == "--no-card"),
        patch_error: None,
    };
    let available = Ok(match model {
        l6fw::Model::L6 => vec!["midi-sync"],
        l6fw::Model::L6max => patch_core::PATCHES.to_vec(),
    });
    let platform = current_platform(true);
    let text_system = platform.text_system();
    let mut app =
        HeadlessAppContext::with_platform(text_system, Arc::new(gpui_kit_assets::Assets), || {
            current_headless_renderer()
        });
    let audio = option(&args, "--window") == Some("audio");
    let height = option(&args, "--height")
        .map(str::parse::<f32>)
        .transpose()?
        .unwrap_or(if audio { 520.0 } else { PATCHES_HEIGHT });
    let debug = matches!(
        option(&args, "--window"),
        Some("debug-heap" | "debug-tasks" | "debug-scheduler")
    );
    let handle = if debug {
        open_debug_window(
            &mut app,
            model,
            option(&args, "--window").unwrap_or("debug-heap").to_owned(),
            option(&args, "--width")
                .map(str::parse::<f32>)
                .transpose()?
                .unwrap_or(760.),
        )?
    } else if audio {
        open_audio_window(&mut app, model, height)?
    } else {
        open_patches_window(&mut app, height, &args, status, available, selection)?
    };
    app.run_until_parked();
    let image = app.capture_screenshot(handle.into())?;
    image.save(output)?;
    println!(
        "Saved {}x{} {} window to {output}",
        image.width(),
        image.height(),
        if debug {
            "FreeRTOS"
        } else if audio {
            "channels and playback"
        } else {
            "patches"
        }
    );
    Ok(())
}
