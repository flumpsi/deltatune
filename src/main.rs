use anyhow::Result;
use minifb::{Window, WindowOptions};
use mpris::{PlaybackStatus, PlayerFinder};
use serde::{Deserialize, Serialize};
use smithay_client_toolkit::{
    compositor::{CompositorHandler, CompositorState},
    delegate_compositor, delegate_layer, delegate_output, delegate_registry, delegate_shm,
    output::{OutputHandler, OutputState},
    registry::{ProvidesRegistryState, RegistryState},
    registry_handlers,
    shell::wlr_layer::{
        Anchor, KeyboardInteractivity, Layer, LayerShell, LayerShellHandler, LayerSurface,
        LayerSurfaceConfigure,
    },
    shell::WaylandSurface,
    shm::{slot::SlotPool, Shm, ShmHandler},
};
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, Sender};
use std::time::{Duration, Instant, SystemTime};
use gtk::glib::{self, ControlFlow, Propagation};
use gtk::prelude::*;
use tray_icon::menu::{Menu, MenuEvent, MenuItem};
use tray_icon::{Icon, TrayIconBuilder};
use wayland_client::{
    globals::registry_queue_init,
    protocol::{wl_output, wl_shm, wl_surface},
    Connection, QueueHandle,
};

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().collect();
    let settings_path = get_arg_value(&args, "--settings-path")
        .map(PathBuf::from)
        .unwrap_or_else(default_settings_path);

    let settings = Settings::load(&settings_path).unwrap_or_default();
    if !settings_path.exists() {
        if let Err(err) = ensure_settings_parent(&settings_path) {
            eprintln!("Failed to create settings directory: {err}");
        } else if let Ok(json) = serde_json::to_string_pretty(&settings) {
            if let Err(err) = std::fs::write(&settings_path, json) {
                eprintln!("Failed to write default settings: {err}");
            }
        }
    }
    let settings_state = SettingsState::new(&settings_path);

    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || mpris_loop(tx));

    start_tray(settings_path.clone());

    let has_wayland = std::env::var("WAYLAND_DISPLAY").map(|v| !v.is_empty()).unwrap_or(false);
    let has_x11 = std::env::var("DISPLAY").map(|v| !v.is_empty()).unwrap_or(false);

    if has_wayland {
        run_wayland(settings_path, settings, settings_state, rx)
    } else if has_x11 {
        run_x11(settings_path, settings, settings_state, rx)
    } else {
        eprintln!("No supported display server found (WAYLAND_DISPLAY or DISPLAY).");
        Ok(())
    }
}

fn run_wayland(
    settings_path: PathBuf,
    settings: Settings,
    settings_state: SettingsState,
    rx: Receiver<MediaInfo>,
) -> Result<()> {
    let conn = Connection::connect_to_env()?;
    let (globals, mut event_queue) = registry_queue_init(&conn)?;
    let qh = event_queue.handle();

    let compositor = CompositorState::bind(&globals, &qh).expect("wl_compositor unavailable");
    let layer_shell = LayerShell::bind(&globals, &qh).expect("layer-shell unavailable");
    let shm = Shm::bind(&globals, &qh).expect("wl_shm unavailable");

    let surface = compositor.create_surface(&qh);
    let layer = layer_shell.create_layer_surface(&qh, surface, Layer::Overlay, Some("deltatune"), None);
    layer.set_anchor(Anchor::TOP | Anchor::LEFT);
    layer.set_margin(settings.y_pos, 0, 0, settings.x_pos);
    layer.set_keyboard_interactivity(KeyboardInteractivity::None);
    layer.set_exclusive_zone(-1);
    layer.set_size(1, 1);
    layer.commit();

    let (font, atlas) = load_assets();
    let pool = SlotPool::new(4, &shm).expect("Failed to create slot pool");

    let mut app = OverlayApp {
        registry_state: RegistryState::new(&globals),
        output_state: OutputState::new(&globals, &qh),
        shm,
        layer,
        pool,
        width: 1,
        height: 1,
        exit: false,
        first_configure: true,
        last_frame: Instant::now(),
        settings_path,
        settings,
        settings_state,
        font,
        atlas,
        media: MediaState::default(),
        media_rx: rx,
        display: DisplayController::new(),
    };

    loop {
        event_queue.blocking_dispatch(&mut app)?;
        if app.exit {
            break;
        }
    }

    Ok(())
}

fn run_x11(
    settings_path: PathBuf,
    settings: Settings,
    settings_state: SettingsState,
    rx: Receiver<MediaInfo>,
) -> Result<()> {
    let (font, atlas) = load_assets();
    let mut app = X11App::new(settings_path, settings, settings_state, font, atlas, rx);

    app.draw();

    let mut window_w = app.width.max(1);
    let mut window_h = app.height.max(1);
    let mut window = Window::new(
        "DeltaTune",
        window_w as usize,
        window_h as usize,
        WindowOptions::default(),
    )?;
    window.limit_update_rate(Some(Duration::from_micros(16_666)));

    while window.is_open() {
        app.draw();

        if app.width != window_w || app.height != window_h {
            window_w = app.width.max(1);
            window_h = app.height.max(1);
            window = Window::new(
                "DeltaTune",
                window_w as usize,
                window_h as usize,
                WindowOptions::default(),
            )?;
            window.limit_update_rate(Some(Duration::from_micros(16_666)));
        }

        window.update_with_buffer(&app.pixels, window_w as usize, window_h as usize)?;
    }

    Ok(())
}

fn load_assets() -> (BitmapFont, FontAtlas) {
    let font_path = PathBuf::from("/usr/share/deltatune/MusicTitleFont.fnt");
    let texture_path = PathBuf::from("/usr/share/deltatune/MusicTitleFont.png");
    // If the font fails to load, we are probably running in a development environment.
    let (font_path, texture_path) = if !font_path.exists() || !texture_path.exists() {
        (
            PathBuf::from("assets/MusicTitleFont.fnt"),
            PathBuf::from("assets/MusicTitleFont.png"),
        )
    } else {
        (font_path, texture_path)
    };
    let mut font = load_bitmap_font(&font_path).unwrap_or_else(|_| BitmapFont::fallback());
    let atlas = FontAtlas::load(&texture_path, &mut font).unwrap_or_else(|_| FontAtlas::empty());
    (font, atlas)
}

fn default_settings_path() -> PathBuf {
    std::env::var("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("."))
        .join(".config")
        .join("deltatune")
        .join("Settings.json")
}

fn get_arg_value(args: &[String], name: &str) -> Option<String> {
    args.iter()
        .position(|arg| arg == name)
        .and_then(|index| args.get(index + 1))
        .cloned()
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
struct Settings {
    scale_factor: f32,
    scale_x: f32,
    scale_y: f32,
    text_scale: f32,
    x_pos: i32,
    y_pos: i32,
    show_artist_name: bool,
    show_playback_status: bool,
    show_debug_overlay: bool,
    force_opaque_background: bool,
    background_opacity: f32,
    hyprland_pin: bool,
    hide_automatically: Option<f32>,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            scale_factor: 3.0,
            scale_x: 1.0,
            scale_y: 1.0,
            text_scale: 1.0,
            x_pos: 0,
            y_pos: 0,
            show_artist_name: true,
            show_playback_status: false,
            show_debug_overlay: false,
            force_opaque_background: false,
            background_opacity: 0.0,
            hyprland_pin: false,
            hide_automatically: Some(2.5),
        }
    }
}

impl Settings {
    fn load(path: &Path) -> Result<Self> {
        if !path.exists() {
            return Ok(Self::default());
        }
        let data = fs::read_to_string(path)?;
        let settings = serde_json::from_str(&data)?;
        Ok(settings)
    }
}

fn ensure_settings_parent(path: &Path) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
    }
    Ok(())
}

struct SettingsState {
    last_modified: Option<SystemTime>,
    last_check: Instant,
}

impl SettingsState {
    fn new(path: &Path) -> Self {
        let last_modified = fs::metadata(path).and_then(|meta| meta.modified()).ok();
        Self {
            last_modified,
            last_check: Instant::now(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum MediaStatus {
    Playing,
    Paused,
    Stopped,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct MediaInfo {
    title: String,
    artist: String,
    status: MediaStatus,
}

impl Default for MediaInfo {
    fn default() -> Self {
        Self {
            title: String::new(),
            artist: String::new(),
            status: MediaStatus::Stopped,
        }
    }
}

struct MediaState {
    info: MediaInfo,
    last_update: Instant,
}

impl Default for MediaState {
    fn default() -> Self {
        Self {
            info: MediaInfo::default(),
            last_update: Instant::now(),
        }
    }
}

fn mpris_loop(tx: Sender<MediaInfo>) {
    let mut last_sent = MediaInfo::default();
    loop {
        match PlayerFinder::new() {
            Ok(finder) => {
                let players = finder.find_all().unwrap_or_default();
                let mut best: Option<MediaInfo> = None;
                for player in players {
                    let status = match player.get_playback_status() {
                        Ok(status) => status,
                        Err(_) => continue,
                    };
                    let metadata = player.get_metadata().ok();
                    let title = metadata
                        .as_ref()
                        .and_then(|m| m.title())
                        .unwrap_or("")
                        .to_string();
                    let artist = metadata
                        .as_ref()
                        .and_then(|m| m.artists())
                        .map(|artists| artists.join(", "))
                        .unwrap_or_default();

                    let info = MediaInfo {
                        title,
                        artist,
                        status: map_status(status),
                    };

                    if best.as_ref().map_or(true, |current| is_better(&info, current)) {
                        best = Some(info);
                    }
                }

                let next = best.unwrap_or_default();
                if next != last_sent {
                    let _ = tx.send(next.clone());
                    last_sent = next;
                }
            }
            Err(_) => {}
        }

        std::thread::sleep(Duration::from_millis(500));
    }
}

fn map_status(status: PlaybackStatus) -> MediaStatus {
    match status {
        PlaybackStatus::Playing => MediaStatus::Playing,
        PlaybackStatus::Paused => MediaStatus::Paused,
        PlaybackStatus::Stopped => MediaStatus::Stopped,
    }
}

fn status_rank(status: &MediaStatus) -> i32 {
    match status {
        MediaStatus::Playing => 3,
        MediaStatus::Paused => 2,
        MediaStatus::Stopped => 1,
    }
}

fn is_better(candidate: &MediaInfo, current: &MediaInfo) -> bool {
    status_rank(&candidate.status) > status_rank(&current.status)
}

fn start_tray(settings_path: PathBuf) {
    std::thread::spawn(move || {
        if let Err(err) = tray_thread(settings_path) {
            eprintln!("Failed to start tray icon: {err}");
        }
    });
}

fn tray_thread(settings_path: PathBuf) -> anyhow::Result<()> {
    gtk::init()?;

    let menu = Menu::new();

    let reload_item = MenuItem::new("Reload settings", true, None);
    let reload_id = reload_item.id().clone();
    menu.append(&reload_item)?;

    let settings_item = MenuItem::new("Settings…", true, None);
    let settings_id = settings_item.id().clone();
    menu.append(&settings_item)?;

    let quit_item = MenuItem::new("Quit", true, None);
    let quit_id = quit_item.id().clone();
    menu.append(&quit_item)?;

    let icon_image =
        image::RgbaImage::from_pixel(32, 32, image::Rgba([255, 255, 255, 255]));
    let icon = Icon::from_rgba(icon_image.into_raw(), 32, 32)?;

    let tray = TrayIconBuilder::new()
        .with_menu(Box::new(menu))
        .with_icon(icon)
        .with_tooltip("DeltaTune")
        .build()?;

    let settings_window = build_settings_window(settings_path.clone())?;

    let settings_path_for_handler = settings_path.clone();
    let menu_events = MenuEvent::receiver();

    glib::timeout_add_local(Duration::from_millis(100), move || {
        while let Ok(event) = menu_events.try_recv() {
            if event.id == quit_id {
                std::process::exit(0);
            }

            if event.id == reload_id {
                let _ = ensure_settings_parent(&settings_path_for_handler).and_then(|_| {
                    std::fs::write(
                        &settings_path_for_handler,
                        std::fs::read_to_string(&settings_path_for_handler).unwrap_or_default(),
                    )
                });
            }

            if event.id == settings_id {
                settings_window.show_all();
                settings_window.present();
            }
        }
        ControlFlow::Continue
    });

    gtk::main();

    drop(tray);

    Ok(())
}

fn build_settings_window(settings_path: PathBuf) -> anyhow::Result<gtk::Window> {
    use gtk::{Adjustment, Box as GtkBox, Button, CheckButton, Label, Orientation, SpinButton, Window, WindowType};

    let settings = Settings::load(&settings_path).unwrap_or_default();

    let window = Window::new(WindowType::Toplevel);
    window.set_title("DeltaTune Settings");
    window.set_default_size(360, 480);
    window.connect_delete_event(|win, _| {
        win.hide();
        Propagation::Stop
    });

    let vbox = GtkBox::new(Orientation::Vertical, 12);
    vbox.set_margin_top(12);
    vbox.set_margin_bottom(12);
    vbox.set_margin_start(12);
    vbox.set_margin_end(12);

    let add_spin = |label: &str, value: f32, min: f64, max: f64, step: f64| -> (GtkBox, SpinButton) {
        let row = GtkBox::new(Orientation::Horizontal, 8);
        let lbl = Label::new(Some(label));
        lbl.set_halign(gtk::Align::Start);
        let adj = Adjustment::new(value as f64, min, max, step, step * 5.0, 0.0);
        let spin = SpinButton::new(Some(&adj), 1.0, 2);
        spin.set_hexpand(true);
        row.pack_start(&lbl, false, false, 0);
        row.pack_end(&spin, false, false, 0);
        (row, spin)
    };

    let (row_scale_factor, spin_scale_factor) = add_spin("Scale factor", settings.scale_factor, 0.1, 10.0, 0.1);
    let (row_scale_x, spin_scale_x) = add_spin("Scale X", settings.scale_x, 0.1, 10.0, 0.1);
    let (row_scale_y, spin_scale_y) = add_spin("Scale Y", settings.scale_y, 0.1, 10.0, 0.1);
    let (row_text_scale, spin_text_scale) = add_spin("Text scale", settings.text_scale, 0.1, 10.0, 0.1);
    let (row_x_pos, spin_x_pos) = add_spin("X position", settings.x_pos as f32, -5000.0, 5000.0, 1.0);
    let (row_y_pos, spin_y_pos) = add_spin("Y position", settings.y_pos as f32, -5000.0, 5000.0, 1.0);

    let check_artist = CheckButton::with_label("Show artist name");
    check_artist.set_active(settings.show_artist_name);

    let check_status = CheckButton::with_label("Show playback status");
    check_status.set_active(settings.show_playback_status);

    let check_debug = CheckButton::with_label("Show debug overlay");
    check_debug.set_active(settings.show_debug_overlay);

    let check_force_opaque = CheckButton::with_label("Force opaque background");
    check_force_opaque.set_active(settings.force_opaque_background);

    let (row_bg_opacity, spin_bg_opacity) =
        add_spin("Background opacity", settings.background_opacity, 0.0, 1.0, 0.05);

    let check_pin = CheckButton::with_label("Hyprland pin");
    check_pin.set_active(settings.hyprland_pin);

    let hide_row = GtkBox::new(Orientation::Horizontal, 8);
    let check_hide = CheckButton::with_label("Hide automatically (seconds)");
    let hide_adj = Adjustment::new(settings.hide_automatically.unwrap_or(2.5) as f64, 0.5, 30.0, 0.5, 2.5, 0.0);
    let spin_hide = SpinButton::new(Some(&hide_adj), 1.0, 2);
    if settings.hide_automatically.is_some() {
        check_hide.set_active(true);
    } else {
        spin_hide.set_sensitive(false);
    }
    hide_row.pack_start(&check_hide, false, false, 0);
    hide_row.pack_end(&spin_hide, false, false, 0);

    check_hide.connect_toggled(glib::clone!(@weak spin_hide => move |toggle| {
        spin_hide.set_sensitive(toggle.is_active());
    }));

    let buttons = GtkBox::new(Orientation::Horizontal, 8);
    buttons.set_halign(gtk::Align::End);
    let save_button = Button::with_label("Save");
    let close_button = Button::with_label("Close");
    buttons.pack_start(&close_button, false, false, 0);
    buttons.pack_start(&save_button, false, false, 0);

    vbox.pack_start(&row_scale_factor, false, false, 0);
    vbox.pack_start(&row_scale_x, false, false, 0);
    vbox.pack_start(&row_scale_y, false, false, 0);
    vbox.pack_start(&row_text_scale, false, false, 0);
    vbox.pack_start(&row_x_pos, false, false, 0);
    vbox.pack_start(&row_y_pos, false, false, 0);
    vbox.pack_start(&check_artist, false, false, 0);
    vbox.pack_start(&check_status, false, false, 0);
    vbox.pack_start(&check_debug, false, false, 0);
    vbox.pack_start(&check_force_opaque, false, false, 0);
    vbox.pack_start(&row_bg_opacity, false, false, 0);
    vbox.pack_start(&check_pin, false, false, 0);
    vbox.pack_start(&hide_row, false, false, 0);
    vbox.pack_start(&buttons, false, false, 0);

    window.add(&vbox);

    let settings_path_for_save = settings_path.clone();
    save_button.connect_clicked(glib::clone!(@weak window,
        @weak spin_scale_factor,
        @weak spin_scale_x,
        @weak spin_scale_y,
        @weak spin_text_scale,
        @weak check_artist,
        @weak check_status,
        @weak check_debug,
        @weak check_force_opaque,
        @weak spin_bg_opacity,
        @weak check_pin,
        @weak check_hide,
        @weak spin_hide
        => move |_| {
            let new_settings = Settings {
                scale_factor: spin_scale_factor.value() as f32,
                scale_x: spin_scale_x.value() as f32,
                scale_y: spin_scale_y.value() as f32,
                text_scale: spin_text_scale.value() as f32,
                x_pos: spin_x_pos.value() as i32,
                y_pos: spin_y_pos.value() as i32,
                show_artist_name: check_artist.is_active(),
                show_playback_status: check_status.is_active(),
                show_debug_overlay: check_debug.is_active(),
                force_opaque_background: check_force_opaque.is_active(),
                background_opacity: spin_bg_opacity.value() as f32,
                hyprland_pin: check_pin.is_active(),
                hide_automatically: if check_hide.is_active() {
                    Some(spin_hide.value() as f32)
                } else {
                    None
                },
            };

            match serde_json::to_string_pretty(&new_settings) {
                Ok(json) => {
                    if let Err(err) = ensure_settings_parent(&settings_path_for_save)
                        .and_then(|_| std::fs::write(&settings_path_for_save, json))
                    {
                        eprintln!("Failed to save settings: {err}");
                    }
                }
                Err(err) => eprintln!("Failed to serialize settings: {err}"),
            }

            window.hide();
        }));

    close_button.connect_clicked(glib::clone!(@weak window => move |_| {
        window.hide();
    }));

    Ok(window)
}

#[derive(Debug, Clone)]
struct Glyph {
    x: f32,
    y: f32,
    width: f32,
    height: f32,
    x_offset: f32,
    y_offset: f32,
    x_advance: f32,
}

#[derive(Debug, Clone)]
struct BitmapFont {
    line_height: f32,
    texture_width: f32,
    texture_height: f32,
    glyphs: HashMap<u32, Glyph>,
    space_advance: f32,
}

impl BitmapFont {
    fn fallback() -> Self {
        Self {
            line_height: 32.0,
            texture_width: 256.0,
            texture_height: 256.0,
            glyphs: HashMap::new(),
            space_advance: 8.0,
        }
    }

    fn set_texture_size(&mut self, width: f32, height: f32) {
        self.texture_width = width;
        self.texture_height = height;
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum TextAnchor {
    TopLeft,
    TopRight,
    BottomLeft,
    BottomRight,
}

impl Default for TextAnchor {
    fn default() -> Self {
        Self::TopLeft
    }
}

struct FontAtlas {
    pixels: Vec<u8>,
    width: u32,
    height: u32,
}

impl FontAtlas {
    fn load(path: &Path, font: &mut BitmapFont) -> Result<Self> {
        let image = image::open(path)?.to_rgba8();
        let (width, height) = image.dimensions();
        font.set_texture_size(width as f32, height as f32);
        Ok(Self {
            pixels: image.into_raw(),
            width,
            height,
        })
    }

    fn empty() -> Self {
        Self {
            pixels: vec![0, 0, 0, 0],
            width: 1,
            height: 1,
        }
    }
}

fn load_bitmap_font(path: &Path) -> Result<BitmapFont> {
    let content = fs::read_to_string(path)?;
    let mut line_height = 48.0;
    let mut tex_w = 512.0;
    let mut tex_h = 512.0;
    let mut glyphs = HashMap::new();

    for line in content.lines() {
        if line.starts_with("common ") {
            let values = parse_kv(line);
            if let Some(value) = values.get("lineHeight") {
                line_height = value.parse::<f32>().unwrap_or(line_height);
            }
            if let Some(value) = values.get("scaleW") {
                tex_w = value.parse::<f32>().unwrap_or(tex_w);
            }
            if let Some(value) = values.get("scaleH") {
                tex_h = value.parse::<f32>().unwrap_or(tex_h);
            }
        } else if line.starts_with("char ") {
            let values = parse_kv(line);
            let id = values
                .get("id")
                .and_then(|v| v.parse::<u32>().ok())
                .unwrap_or(0);
            let glyph = Glyph {
                x: values.get("x").and_then(|v| v.parse::<f32>().ok()).unwrap_or(0.0),
                y: values.get("y").and_then(|v| v.parse::<f32>().ok()).unwrap_or(0.0),
                width: values
                    .get("width")
                    .and_then(|v| v.parse::<f32>().ok())
                    .unwrap_or(0.0),
                height: values
                    .get("height")
                    .and_then(|v| v.parse::<f32>().ok())
                    .unwrap_or(0.0),
                x_offset: values
                    .get("xoffset")
                    .and_then(|v| v.parse::<f32>().ok())
                    .unwrap_or(0.0),
                y_offset: values
                    .get("yoffset")
                    .and_then(|v| v.parse::<f32>().ok())
                    .unwrap_or(0.0),
                x_advance: values
                    .get("xadvance")
                    .and_then(|v| v.parse::<f32>().ok())
                    .unwrap_or(0.0),
            };
            glyphs.insert(id, glyph);
        }
    }

    let space_advance = glyphs
        .get(&32)
        .map(|g| g.x_advance)
        .unwrap_or(line_height * 0.25);

    Ok(BitmapFont {
        line_height,
        texture_width: tex_w,
        texture_height: tex_h,
        glyphs,
        space_advance,
    })
}

fn parse_kv(line: &str) -> HashMap<&str, &str> {
    let mut map = HashMap::new();
    for token in line.split_whitespace().skip(1) {
        if let Some((key, value)) = token.split_once('=') {
            map.insert(key, value.trim_matches('"'));
        }
    }
    map
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DisplayState {
    Hidden,
    AppearingDelay,
    Appearing,
    Visible,
    Disappearing,
}

struct DisplaySlot {
    text: String,
    state: DisplayState,
    timer: f32,
    opacity: f32,
    offset_x: f32,
}

struct DisplayController {
    slots: [DisplaySlot; 2],
    primary_index: usize,
    current_media: MediaInfo,
}

impl DisplayController {
    fn new() -> Self {
        Self {
            slots: [
                DisplaySlot {
                    text: String::new(),
                    state: DisplayState::Hidden,
                    timer: 0.0,
                    opacity: 0.0,
                    offset_x: 0.0,
                },
                DisplaySlot {
                    text: String::new(),
                    state: DisplayState::Hidden,
                    timer: 0.0,
                    opacity: 0.0,
                    offset_x: 0.0,
                },
            ],
            primary_index: 0,
            current_media: MediaInfo::default(),
        }
    }
}

struct X11App {
    width: u32,
    height: u32,
    last_frame: Instant,
    settings_path: PathBuf,
    settings: Settings,
    settings_state: SettingsState,
    font: BitmapFont,
    atlas: FontAtlas,
    media: MediaState,
    media_rx: Receiver<MediaInfo>,
    display: DisplayController,
    canvas: Vec<u8>,
    pixels: Vec<u32>,
}

impl X11App {
    fn new(
        settings_path: PathBuf,
        settings: Settings,
        settings_state: SettingsState,
        font: BitmapFont,
        atlas: FontAtlas,
        media_rx: Receiver<MediaInfo>,
    ) -> Self {
        Self {
            width: 1,
            height: 1,
            last_frame: Instant::now(),
            settings_path,
            settings,
            settings_state,
            font,
            atlas,
            media: MediaState::default(),
            media_rx,
            display: DisplayController::new(),
            canvas: Vec::new(),
            pixels: Vec::new(),
        }
    }

    fn draw(&mut self) {
        let now = Instant::now();
        let dt = now.duration_since(self.last_frame).as_secs_f32();
        self.last_frame = now;

        self.poll_media_updates();
        self.reload_settings_if_needed();
        self.update_display_state(dt);

        let scale = self.settings.scale_factor * self.settings.text_scale;
        let padding = 12.0;

        let mut max_width: f32 = 1.0;
        let mut max_height: f32 = self.font.line_height * scale;
        for slot in self.display.slots.iter() {
            if slot.state == DisplayState::Hidden || slot.opacity <= 0.0 || slot.text.is_empty() {
                continue;
            }
            let (w, h) = measure_text(&slot.text, &self.font, scale);
            max_width = max_width.max(w);
            max_height = max_height.max(h);
        }

        let desired_width = ((max_width + padding * 2.0) * self.settings.scale_x)
            .max(1.0)
            .round() as u32;
        let desired_height = ((max_height + padding * 2.0) * self.settings.scale_y)
            .max(1.0)
            .round() as u32;

        if desired_width != self.width || desired_height != self.height {
            self.width = desired_width;
            self.height = desired_height;
        }

        let needed = (self.width * self.height * 4) as usize;
        if self.canvas.len() != needed {
            self.canvas.resize(needed, 0);
        }

        fill_background(
            &mut self.canvas,
            self.settings.force_opaque_background,
            self.settings.background_opacity,
        );

        for slot in self.display.slots.iter() {
            if slot.state == DisplayState::Hidden || slot.opacity <= 0.0 || slot.text.is_empty() {
                continue;
            }
            let origin_x = padding + slot.offset_x;
            let origin_y = padding;
            draw_text(
                &mut self.canvas,
                self.width,
                self.height,
                &self.font,
                &self.atlas,
                &slot.text,
                scale,
                origin_x,
                origin_y,
                slot.opacity,
            );
        }

        pack_bgra_to_xrgb(&self.canvas, &mut self.pixels);
    }

    fn poll_media_updates(&mut self) {
        while let Ok(info) = self.media_rx.try_recv() {
            self.media.info = info;
            self.media.last_update = Instant::now();
        }
    }

    fn reload_settings_if_needed(&mut self) {
        if self.settings_state.last_check.elapsed() < Duration::from_millis(500) {
            return;
        }
        self.settings_state.last_check = Instant::now();

        let metadata = match fs::metadata(&self.settings_path) {
            Ok(metadata) => metadata,
            Err(_) => return,
        };
        let modified = metadata.modified().ok();
        if modified.is_none() || modified == self.settings_state.last_modified {
            return;
        }

        if let Ok(settings) = Settings::load(&self.settings_path) {
            self.settings = settings;
        }
        self.settings_state.last_modified = modified;
    }

    fn update_display_state(&mut self, dt: f32) {
        let mut title_changed = false;
        let mut artist_changed = false;
        let mut status_changed = false;

        if self.display.current_media != self.media.info {
            title_changed = self.display.current_media.title != self.media.info.title;
            artist_changed = self.display.current_media.artist != self.media.info.artist;
            status_changed = self.display.current_media.status != self.media.info.status;
            self.display.current_media = self.media.info.clone();
        }

        let mut should_update = if self.settings.show_playback_status {
            title_changed || artist_changed || status_changed
        } else {
            title_changed || artist_changed
        };

        if !should_update
            && !self.settings.show_playback_status
            && status_changed
            && self.media.info.status == MediaStatus::Playing
        {
            should_update = true;
        }

        let primary_index = self.display.primary_index;
        if !self.settings.show_playback_status
            && should_update
            && self.media.info.status != MediaStatus::Playing
            && self.display.slots[primary_index].state == DisplayState::Hidden
        {
            should_update = false;
        }

        if should_update {
            match self.display.slots[primary_index].state {
                DisplayState::Hidden => swap_and_show(&mut self.display, &self.settings),
                DisplayState::Visible => {
                    if title_changed || artist_changed {
                        swap_and_show(&mut self.display, &self.settings)
                    }
                }
                DisplayState::Disappearing => swap_and_show(&mut self.display, &self.settings),
                DisplayState::AppearingDelay | DisplayState::Appearing => {}
            }
        }

        for slot in self.display.slots.iter_mut() {
            update_display_slot(slot, &self.settings, &self.media, dt);
        }
    }
}

struct OverlayApp {
    registry_state: RegistryState,
    output_state: OutputState,
    shm: Shm,
    layer: LayerSurface,
    pool: SlotPool,
    width: u32,
    height: u32,
    exit: bool,
    first_configure: bool,
    last_frame: Instant,
    settings_path: PathBuf,
    settings: Settings,
    settings_state: SettingsState,
    font: BitmapFont,
    atlas: FontAtlas,
    media: MediaState,
    media_rx: Receiver<MediaInfo>,
    display: DisplayController,
}

impl OverlayApp {
    fn draw(&mut self, qh: &QueueHandle<Self>) {
        let now = Instant::now();
        let dt = now.duration_since(self.last_frame).as_secs_f32();
        self.last_frame = now;

        self.poll_media_updates();
        self.reload_settings_if_needed();
        self.update_display_state(dt);

        let scale = self.settings.scale_factor * self.settings.text_scale;
        let padding = 12.0;

        let mut max_width: f32 = 1.0;
        let mut max_height: f32 = self.font.line_height * scale;
        for slot in self.display.slots.iter() {
            if slot.state == DisplayState::Hidden || slot.opacity <= 0.0 || slot.text.is_empty() {
                continue;
            }
            let (w, h) = measure_text(&slot.text, &self.font, scale);
            max_width = max_width.max(w);
            max_height = max_height.max(h);
        }

        let desired_width = ((max_width + padding * 2.0) * self.settings.scale_x)
            .max(1.0)
            .round() as u32;
        let desired_height = ((max_height + padding * 2.0) * self.settings.scale_y)
            .max(1.0)
            .round() as u32;

        if desired_width != self.width || desired_height != self.height {
            self.width = desired_width;
            self.height = desired_height;
            self.layer.set_size(self.width, self.height);
        }

        let stride = self.width as i32 * 4;
        let (buffer, canvas) = self
            .pool
            .create_buffer(self.width as i32, self.height as i32, stride, wl_shm::Format::Argb8888)
            .expect("create buffer");

        fill_background(
            canvas,
            self.settings.force_opaque_background,
            self.settings.background_opacity,
        );

        for slot in self.display.slots.iter() {
            if slot.state == DisplayState::Hidden || slot.opacity <= 0.0 || slot.text.is_empty() {
                continue;
            }
            let origin_x = padding + slot.offset_x;
            let origin_y = padding;
            draw_text(
                canvas,
                self.width,
                self.height,
                &self.font,
                &self.atlas,
                &slot.text,
                scale,
                origin_x,
                origin_y,
                slot.opacity,
            );
        }

        self.layer
            .wl_surface()
            .damage_buffer(0, 0, self.width as i32, self.height as i32);
        self.layer.wl_surface().frame(qh, self.layer.wl_surface().clone());
        self.layer
            .set_margin(self.settings.y_pos, 0, 0, self.settings.x_pos);
        buffer.attach_to(self.layer.wl_surface()).expect("buffer attach");
        self.layer.commit();
    }

    fn poll_media_updates(&mut self) {
        while let Ok(info) = self.media_rx.try_recv() {
            self.media.info = info;
            self.media.last_update = Instant::now();
        }
    }

    fn reload_settings_if_needed(&mut self) {
        if self.settings_state.last_check.elapsed() < Duration::from_millis(500) {
            return;
        }
        self.settings_state.last_check = Instant::now();

        let metadata = match fs::metadata(&self.settings_path) {
            Ok(metadata) => metadata,
            Err(_) => return,
        };
        let modified = metadata.modified().ok();
        if modified.is_none() || modified == self.settings_state.last_modified {
            return;
        }

        if let Ok(settings) = Settings::load(&self.settings_path) {
            self.settings = settings;
        }
        self.settings_state.last_modified = modified;
    }

    fn update_display_state(&mut self, dt: f32) {
        let mut title_changed = false;
        let mut artist_changed = false;
        let mut status_changed = false;

        if self.display.current_media != self.media.info {
            title_changed = self.display.current_media.title != self.media.info.title;
            artist_changed = self.display.current_media.artist != self.media.info.artist;
            status_changed = self.display.current_media.status != self.media.info.status;
            self.display.current_media = self.media.info.clone();
        }

        let mut should_update = if self.settings.show_playback_status {
            title_changed || artist_changed || status_changed
        } else {
            title_changed || artist_changed
        };

        if !should_update
            && !self.settings.show_playback_status
            && status_changed
            && self.media.info.status == MediaStatus::Playing
        {
            should_update = true;
        }

        let primary_index = self.display.primary_index;
        if !self.settings.show_playback_status
            && should_update
            && self.media.info.status != MediaStatus::Playing
            && self.display.slots[primary_index].state == DisplayState::Hidden
        {
            should_update = false;
        }

        if should_update {
            match self.display.slots[primary_index].state {
                DisplayState::Hidden => swap_and_show(&mut self.display, &self.settings),
                DisplayState::Visible => {
                    if title_changed || artist_changed {
                        swap_and_show(&mut self.display, &self.settings)
                    }
                }
                DisplayState::Disappearing => swap_and_show(&mut self.display, &self.settings),
                DisplayState::AppearingDelay | DisplayState::Appearing => {}
            }
        }

        for slot in self.display.slots.iter_mut() {
            update_display_slot(slot, &self.settings, &self.media, dt);
        }
    }
}

impl CompositorHandler for OverlayApp {
    fn scale_factor_changed(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _surface: &wl_surface::WlSurface,
        _new_factor: i32,
    ) {
    }

    fn transform_changed(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _surface: &wl_surface::WlSurface,
        _new_transform: wl_output::Transform,
    ) {
    }

    fn frame(
        &mut self,
        _conn: &Connection,
        qh: &QueueHandle<Self>,
        _surface: &wl_surface::WlSurface,
        _time: u32,
    ) {
        self.draw(qh);
    }

    fn surface_enter(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _surface: &wl_surface::WlSurface,
        _output: &wl_output::WlOutput,
    ) {
    }

    fn surface_leave(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _surface: &wl_surface::WlSurface,
        _output: &wl_output::WlOutput,
    ) {
    }
}

impl OutputHandler for OverlayApp {
    fn output_state(&mut self) -> &mut OutputState {
        &mut self.output_state
    }

    fn new_output(&mut self, _conn: &Connection, _qh: &QueueHandle<Self>, _output: wl_output::WlOutput) {}
    fn update_output(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _output: wl_output::WlOutput,
    ) {
    }
    fn output_destroyed(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _output: wl_output::WlOutput,
    ) {
    }
}

impl ShmHandler for OverlayApp {
    fn shm_state(&mut self) -> &mut Shm {
        &mut self.shm
    }
}

impl LayerShellHandler for OverlayApp {
    fn closed(&mut self, _conn: &Connection, _qh: &QueueHandle<Self>, _layer: &LayerSurface) {
        self.exit = true;
    }

    fn configure(
        &mut self,
        _conn: &Connection,
        qh: &QueueHandle<Self>,
        _layer: &LayerSurface,
        configure: LayerSurfaceConfigure,
        _serial: u32,
    ) {
        if configure.new_size.0 != 0 && configure.new_size.1 != 0 {
            self.width = configure.new_size.0;
            self.height = configure.new_size.1;
        }

        if self.first_configure {
            self.first_configure = false;
            self.draw(qh);
        }
    }
}

impl ProvidesRegistryState for OverlayApp {
    fn registry(&mut self) -> &mut RegistryState {
        &mut self.registry_state
    }
    registry_handlers![OutputState];
}

delegate_compositor!(OverlayApp);
delegate_output!(OverlayApp);

delegate_shm!(OverlayApp);

delegate_layer!(OverlayApp);

delegate_registry!(OverlayApp);

fn swap_and_show(controller: &mut DisplayController, settings: &Settings) {
    let primary_index = controller.primary_index;
    let secondary_index = 1 - primary_index;

    controller.primary_index = secondary_index;
    let new_primary = controller.primary_index;

    let text = format_media_text(settings, &controller.current_media);
    update_slot_text(&mut controller.slots[new_primary], text);

    if controller.slots[secondary_index].state == DisplayState::Hidden {
        controller.slots[new_primary].state = DisplayState::Appearing;
        controller.slots[new_primary].timer = 0.0;
    } else {
        if controller.slots[secondary_index].state != DisplayState::Disappearing {
            controller.slots[secondary_index].state = DisplayState::Disappearing;
            controller.slots[secondary_index].timer = 0.0;
        }
        controller.slots[new_primary].state = DisplayState::AppearingDelay;
        controller.slots[new_primary].timer = 0.0;
    }
}

fn update_display_slot(slot: &mut DisplaySlot, settings: &Settings, media: &MediaState, dt: f32) {
    const APPEAR_DELAY: f32 = 0.5;
    const APPEAR_DURATION: f32 = 0.75;
    const DISAPPEAR_DURATION: f32 = 0.75;
    const STAY_TIME: f32 = 2.5;
    const SLIDE_IN_DISTANCE: f32 = 24.0;
    const SLIDE_OUT_DISTANCE: f32 = 24.0;

    let scale = settings.scale_factor * settings.text_scale;
    match slot.state {
        DisplayState::AppearingDelay => {
            if slot.timer == 0.0 {
                slot.opacity = 0.0;
                slot.offset_x = 0.0;
            }
            if slot.timer >= APPEAR_DELAY {
                slot.state = DisplayState::Appearing;
                slot.timer = 0.0;
            }
        }
        DisplayState::Appearing => {
            if slot.timer == 0.0 {
                slot.opacity = 0.0;
                slot.offset_x = 0.0;
            }
            let progress = (slot.timer / APPEAR_DURATION).clamp(0.0, 1.0);
            slot.opacity = (progress * 1.5 - 0.25).clamp(0.0, 1.0);
            slot.offset_x = interpolate_quadratic(SLIDE_IN_DISTANCE * scale, 0.0, progress);
            if slot.timer >= APPEAR_DURATION {
                slot.state = DisplayState::Visible;
                slot.timer = 0.0;
            }
        }
        DisplayState::Visible => {
            if slot.timer == 0.0 {
                slot.opacity = 1.0;
                slot.offset_x = 0.0;
            }
            if let Some(hide_after) = settings.hide_automatically {
                if slot.timer >= hide_after {
                    slot.state = DisplayState::Disappearing;
                    slot.timer = 0.0;
                }
            } else if !settings.show_playback_status
                && slot.timer >= STAY_TIME
                && (media.info.status == MediaStatus::Stopped || media.info.status == MediaStatus::Paused)
            {
                slot.state = DisplayState::Disappearing;
                slot.timer = 0.0;
            }
        }
        DisplayState::Disappearing => {
            if slot.timer == 0.0 {
                slot.opacity = 1.0;
                slot.offset_x = 0.0;
            }
            let progress = (slot.timer / DISAPPEAR_DURATION).clamp(0.0, 1.0);
            slot.opacity = ((1.0 - progress) * 1.5 - 0.25).clamp(0.0, 1.0);
            slot.offset_x = interpolate_quadratic(-SLIDE_OUT_DISTANCE * scale, 0.0, 1.0 - progress);
            if slot.timer >= DISAPPEAR_DURATION {
                slot.state = DisplayState::Hidden;
                slot.opacity = 0.0;
            }
        }
        DisplayState::Hidden => {
            slot.opacity = 0.0;
            slot.offset_x = 0.0;
        }
    }

    if slot.state != DisplayState::Hidden {
        slot.timer += dt;
    }
}

fn update_slot_text(slot: &mut DisplaySlot, text: String) {
    if slot.text == text {
        return;
    }
    slot.text = text;
}

fn interpolate_quadratic(a: f32, b: f32, t: f32) -> f32 {
    let one_minus_t = 1.0 - t;
    let progress = 1.0 - one_minus_t * one_minus_t;
    a + (b - a) * progress
}

fn format_media_text(settings: &Settings, media: &MediaInfo) -> String {
    if media.status == MediaStatus::Stopped {
        return String::new();
    }

    let mut title = media.title.trim().to_string();
    let mut artist = media.artist.trim().to_string();

    if artist.ends_with(" - Topic") {
        artist.truncate(artist.len().saturating_sub(8));
    }

    if !artist.is_empty() && !title.is_empty() {
        let prefix = format!("{artist} - ");
        if title.starts_with(&prefix) {
            title = title.replacen(&prefix, "", 1);
        }
        let suffix = format!(" - {artist}");
        if title.ends_with(&suffix) {
            title.truncate(title.len().saturating_sub(suffix.len()));
        }
    }

    let mut buffer = String::new();
    if settings.show_playback_status {
        let icon = match media.status {
            MediaStatus::Playing => "♪",
            MediaStatus::Paused => "⏸",
            MediaStatus::Stopped => "",
        };
        if !icon.is_empty() {
            buffer.push_str(icon);
            buffer.push_str("~\u{2009}\u{2009}\u{2009}");
        }
    } else if media.status == MediaStatus::Playing {
        buffer.push_str("♪~\u{2009}\u{2009}\u{2009}");
    }

    if !title.is_empty() {
        buffer.push_str(&title);
    }

    if settings.show_artist_name && !artist.is_empty() {
        if !buffer.is_empty() {
            buffer.push('\n');
        }
        buffer.push_str(&artist);
    }

    buffer
}

fn measure_text(text: &str, font: &BitmapFont, scale: f32) -> (f32, f32) {
    let mut max_width: f32 = 0.0;
    let mut current_width: f32 = 0.0;
    let mut lines = 1;

    for ch in text.chars() {
        if ch == '\n' {
            max_width = max_width.max(current_width);
            current_width = 0.0;
            lines += 1;
            continue;
        }

        if let Some(glyph) = font.glyphs.get(&(ch as u32)) {
            current_width += glyph.x_advance * scale;
        } else {
            current_width += font.space_advance * scale;
        }
    }

    max_width = max_width.max(current_width);
    let height = lines as f32 * font.line_height * scale;
    (max_width, height)
}

fn fill_background(canvas: &mut [u8], force_opaque: bool, opacity: f32) {
    let alpha = if force_opaque { 1.0 } else { opacity.clamp(0.0, 1.0) };
    let a = (alpha * 255.0).round() as u8;
    for chunk in canvas.chunks_exact_mut(4) {
        chunk[0] = 0;
        chunk[1] = 0;
        chunk[2] = 0;
        chunk[3] = a;
    }
}

fn pack_bgra_to_xrgb(src: &[u8], dst: &mut Vec<u32>) {
    let count = src.len() / 4;
    if dst.len() != count {
        dst.resize(count, 0);
    }
    for (i, chunk) in src.chunks_exact(4).enumerate() {
        let b = chunk[0] as u32;
        let g = chunk[1] as u32;
        let r = chunk[2] as u32;
        dst[i] = (r << 16) | (g << 8) | b;
    }
}

fn draw_text(
    canvas: &mut [u8],
    canvas_w: u32,
    canvas_h: u32,
    font: &BitmapFont,
    atlas: &FontAtlas,
    text: &str,
    scale: f32,
    origin_x: f32,
    origin_y: f32,
    opacity: f32,
) {
    let mut cursor_x = origin_x;
    let mut cursor_y = origin_y;

    for ch in text.chars() {
        if ch == '\n' {
            cursor_x = origin_x;
            cursor_y += font.line_height * scale;
            continue;
        }

        let glyph = match font.glyphs.get(&(ch as u32)) {
            Some(glyph) => glyph,
            None => {
                cursor_x += font.space_advance * scale;
                continue;
            }
        };

        let x0 = cursor_x + glyph.x_offset * scale;
        let y0 = cursor_y + glyph.y_offset * scale;
        let dest_w = (glyph.width * scale).round().max(1.0) as i32;
        let dest_h = (glyph.height * scale).round().max(1.0) as i32;

        for dy in 0..dest_h {
            let src_y = ((dy as f32) / scale).floor() as i32;
            if src_y < 0 || src_y >= glyph.height as i32 {
                continue;
            }
            let dest_y = y0.round() as i32 + dy;
            if dest_y < 0 || dest_y >= canvas_h as i32 {
                continue;
            }

            for dx in 0..dest_w {
                let src_x = ((dx as f32) / scale).floor() as i32;
                if src_x < 0 || src_x >= glyph.width as i32 {
                    continue;
                }
                let dest_x = x0.round() as i32 + dx;
                if dest_x < 0 || dest_x >= canvas_w as i32 {
                    continue;
                }

                let tex_x = glyph.x as i32 + src_x;
                let tex_y = glyph.y as i32 + src_y;
                if tex_x < 0
                    || tex_y < 0
                    || tex_x >= atlas.width as i32
                    || tex_y >= atlas.height as i32
                {
                    continue;
                }

                let src_index = ((tex_y as u32 * atlas.width + tex_x as u32) * 4) as usize;
                let src_r = atlas.pixels[src_index];
                let src_g = atlas.pixels[src_index + 1];
                let src_b = atlas.pixels[src_index + 2];
                let src_a = atlas.pixels[src_index + 3];
                if src_a == 0 {
                    continue;
                }

                let dst_index = ((dest_y as u32 * canvas_w + dest_x as u32) * 4) as usize;
                blend_pixel(&mut canvas[dst_index..dst_index + 4], src_r, src_g, src_b, src_a, opacity);
            }
        }

        cursor_x += glyph.x_advance * scale;
    }
}

fn blend_pixel(dst: &mut [u8], src_r: u8, src_g: u8, src_b: u8, src_a: u8, opacity: f32) {
    let sa = (src_a as f32 / 255.0) * opacity;
    if sa <= 0.0 {
        return;
    }
    let sr = (src_r as f32 / 255.0) * sa;
    let sg = (src_g as f32 / 255.0) * sa;
    let sb = (src_b as f32 / 255.0) * sa;

    let db = dst[0] as f32 / 255.0;
    let dg = dst[1] as f32 / 255.0;
    let dr = dst[2] as f32 / 255.0;
    let da = dst[3] as f32 / 255.0;

    let out_a = sa + da * (1.0 - sa);
    let out_r = sr + dr * (1.0 - sa);
    let out_g = sg + dg * (1.0 - sa);
    let out_b = sb + db * (1.0 - sa);

    dst[0] = (out_b * 255.0).round().clamp(0.0, 255.0) as u8;
    dst[1] = (out_g * 255.0).round().clamp(0.0, 255.0) as u8;
    dst[2] = (out_r * 255.0).round().clamp(0.0, 255.0) as u8;
    dst[3] = (out_a * 255.0).round().clamp(0.0, 255.0) as u8;
}
