use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::{broadcast, mpsc};

use eframe::egui;
use egui::ColorImage;
use rustdeck_core::{
    Core,
    config::{NEXT_PAGE_POS, Page, PageNav, PREV_PAGE_POS, Profile, config_dir, default_profile_path, profiles_dir},
    event::{Command, DeviceInfo, DeviceKind, Event},
};

// ── Editor state for the currently selected button ───────────────────────────

#[derive(Default)]
struct ButtonEditor {
    device_id: String,
    position:  u16,
    label:     String,
    icon_path: String,   // display path (may be empty)
    script:    String,   // inline Lua
    dirty:     bool,
}

impl ButtonEditor {
    fn load(device_id: &str, page: usize, position: u16, profile: &Profile) -> Self {
        let btn = profile.button(page, position as u8);
        Self {
            device_id: device_id.to_owned(),
            position,
            label:     btn.and_then(|b| b.label.clone()).unwrap_or_default(),
            icon_path: btn
                .and_then(|b| b.icon.as_ref())
                .map(|p| p.to_string_lossy().into_owned())
                .unwrap_or_default(),
            script:    btn.and_then(|b| b.on_press.clone()).unwrap_or_default(),
            dirty:     false,
        }
    }
}

// ── App ───────────────────────────────────────────────────────────────────────

struct App {
    _core:    Arc<Core>,
    devices:  HashMap<String, DeviceInfo>,
    event_rx: broadcast::Receiver<Event>,
    cmd_tx:   mpsc::Sender<Command>,
    log:      Vec<String>,

    profile:  Profile,
    /// Page shown in the grid (mirrors the device's current page)
    page:     usize,
    textures_dirty: bool,

    /// Which button is selected (device_id, position)
    selected: Option<(String, u16)>,
    editor:   ButtonEditor,

    /// Cached texture for the current icon preview (editor panel)
    icon_texture: Option<egui::TextureHandle>,
    /// Cached textures for grid cells: key = (device_id, position)
    grid_textures: HashMap<(String, u8), egui::TextureHandle>,
}

impl App {
    fn new(core: Arc<Core>, initial_devices: HashMap<String, DeviceInfo>) -> Self {
        let event_rx = core.subscribe();
        let cmd_tx   = core.commands();

        let profile = Profile::load(&default_profile_path()).unwrap_or_default();
        let page = core.current_page().min(profile.pages.len().saturating_sub(1));

        let mut log = Vec::new();
        for d in initial_devices.values() {
            log.push(format!("Connected: {} ({}×{})", d.name, d.rows, d.columns));
        }

        Self {
            _core: core,
            devices: initial_devices,
            event_rx,
            cmd_tx,
            log,
            profile,
            page,
            textures_dirty: true,
            selected: None,
            editor: ButtonEditor::default(),
            icon_texture: None,
            grid_textures: HashMap::new(),
        }
    }

    fn load_all_grid_textures(&mut self, ctx: &egui::Context) {
        self.grid_textures.clear();
        let Some(page) = self.profile.pages.get(self.page) else { return };
        let positions: Vec<(String, u8)> = self.devices.keys()
            .flat_map(|id| page.buttons.keys().map(move |&pos| (id.clone(), pos)))
            .collect();
        for (device_id, pos) in positions {
            self.load_grid_texture(ctx, &device_id, pos);
        }
    }

    /// Load (or reload) the grid texture for a specific button position.
    fn load_grid_texture(&mut self, ctx: &egui::Context, device_id: &str, position: u8) {
        let path = self.profile.button(self.page, position)
            .and_then(|b| b.icon.as_ref())
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_default();

        let key = (device_id.to_owned(), position);
        if path.is_empty() {
            self.grid_textures.remove(&key);
            return;
        }
        if let Ok(img) = image::open(&path) {
            let rgba = img.to_rgba8();
            let (w, h) = rgba.dimensions();
            let pixels = rgba.pixels()
                .map(|p| egui::Color32::from_rgba_unmultiplied(p[0], p[1], p[2], p[3]))
                .collect();
            let ci = ColorImage { size: [w as usize, h as usize], pixels };
            let tex = ctx.load_texture(format!("btn-{device_id}-{position}"), ci, Default::default());
            self.grid_textures.insert(key, tex);
        } else {
            self.grid_textures.remove(&key);
        }
    }

    /// Write the profile to disk and have the core reload it and draw `self.page`.
    fn persist(&mut self) {
        match self.profile.save(&default_profile_path()) {
            Ok(()) => { let _ = self.cmd_tx.try_send(Command::ReloadProfile { page: self.page }); }
            Err(e) => self.push_log(format!("Save error: {e}")),
        }
    }

    /// Follow a page change (from the device or from the GUI) in the grid.
    fn set_page(&mut self, page: usize) {
        if page != self.page {
            self.page = page;
            self.selected = None;
            self.textures_dirty = true;
        }
    }

    fn go_to_page(&mut self, page: usize) {
        self.set_page(page);
        let _ = self.cmd_tx.try_send(Command::ShowPage(page));
    }

    fn add_page(&mut self) {
        let shadowed: Vec<u8> = [PREV_PAGE_POS, NEXT_PAGE_POS].into_iter()
            .filter(|pos| self.profile.pages.iter().any(|p| p.buttons.contains_key(pos)))
            .collect();
        if self.profile.pages.len() == 1 && !shadowed.is_empty() {
            self.push_log(format!("Buttons {shadowed:?} are now page navigation; their actions are hidden."));
        }
        self.profile.pages.push(Page::default());
        self.set_page(self.profile.pages.len() - 1);
        self.persist();
    }

    fn delete_page(&mut self) {
        if self.profile.pages.len() < 2 { return; }
        self.profile.pages.remove(self.page);
        let page = self.page.min(self.profile.pages.len() - 1);
        self.page = usize::MAX; // force set_page to refresh
        self.set_page(page);
        self.persist();
    }

    /// Load the icon at editor.icon_path into a GPU texture for preview.
    fn reload_icon_texture(&mut self, ctx: &egui::Context) {
        let path = self.editor.icon_path.trim().to_owned();
        if path.is_empty() { self.icon_texture = None; return; }
        match image::open(&path) {
            Ok(img) => {
                let rgba = img.to_rgba8();
                let (w, h) = rgba.dimensions();
                let pixels: Vec<egui::Color32> = rgba.pixels()
                    .map(|p| egui::Color32::from_rgba_unmultiplied(p[0], p[1], p[2], p[3]))
                    .collect();
                let color_image = ColorImage { size: [w as usize, h as usize], pixels };
                self.icon_texture = Some(ctx.load_texture("icon_preview", color_image, Default::default()));
            }
            Err(_) => { self.icon_texture = None; }
        }
    }

    fn drain_events(&mut self) {
        while let Ok(ev) = self.event_rx.try_recv() {
            match ev {
                Event::DeviceConnected(ref info) => {
                    self.push_log(format!("Connected: {} ({}×{})", info.name, info.rows, info.columns));
                    self.devices.insert(info.id.clone(), info.clone());
                }
                Event::DeviceDisconnected(ref id) => {
                    self.push_log(format!("Disconnected: {id}"));
                    self.devices.remove(id);
                }
                Event::ButtonPress { ref device, position } => {
                    self.push_log(format!("▼ btn {position} on {device}"));
                }
                Event::ButtonRelease { ref device, position } => {
                    self.push_log(format!("▲ btn {position} on {device}"));
                }
                Event::LuaError { ref context, ref error } => {
                    self.push_log(format!("Lua [{context}]: {error}"));
                }
                Event::PageChanged(page) => self.set_page(page),
                _ => {}
            }
        }
    }

    fn push_log(&mut self, msg: String) {
        self.log.push(msg);
        if self.log.len() > 300 { self.log.remove(0); }
    }

    fn select_button(&mut self, ctx: &egui::Context, device_id: &str, position: u16) {
        self.selected = Some((device_id.to_owned(), position));
        self.editor = ButtonEditor::load(device_id, self.page, position, &self.profile);
        self.icon_texture = None;
        self.reload_icon_texture(ctx);
    }

    fn save_button(&mut self, ctx: &egui::Context) {
        let pos       = self.editor.position as u8;
        let device_id = self.editor.device_id.clone();
        let Some(page) = self.profile.pages.get_mut(self.page) else { return };
        let btn = page.buttons.entry(pos).or_default();
        btn.label    = if self.editor.label.is_empty() { None } else { Some(self.editor.label.clone()) };
        btn.icon     = if self.editor.icon_path.is_empty() { None } else { Some(self.editor.icon_path.clone().into()) };
        btn.on_press = if self.editor.script.is_empty() { None } else { Some(self.editor.script.clone()) };
        self.editor.dirty = false;

        self.persist();
        self.push_log(format!("Saved button {pos} on page {}.", self.page + 1));
        self.load_grid_texture(ctx, &device_id, pos);
    }

    fn open_images_dir(&self) {
        // Open the config dir in the default file manager so the user can find icons
        let dir = config_dir().join("icons");
        let _ = std::fs::create_dir_all(&dir);
        let _ = std::process::Command::new("xdg-open").arg(&dir).spawn();
    }
}

// ── egui rendering ────────────────────────────────────────────────────────────

impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.drain_events();

        if self.textures_dirty && !self.devices.is_empty() {
            self.load_all_grid_textures(ctx);
            self.textures_dirty = false;
        }

        // ── Log panel (bottom) ────────────────────────────────────────────────
        egui::TopBottomPanel::bottom("log")
            .resizable(true)
            .min_height(80.0)
            .default_height(120.0)
            .show(ctx, |ui| {
                ui.label(egui::RichText::new("Log").strong());
                egui::ScrollArea::vertical().stick_to_bottom(true).id_salt("log_scroll").show(ui, |ui| {
                    for line in &self.log {
                        ui.label(egui::RichText::new(line).size(11.0).monospace());
                    }
                });
            });

        // ── Editor panel (right) — only when a button is selected ─────────────
        if self.selected.is_some() {
            egui::SidePanel::right("editor")
                .resizable(true)
                .min_width(240.0)
                .default_width(280.0)
                .show(ctx, |ui| {
                    let pos = self.editor.position;
                    ui.heading(format!("Button {pos} — page {}", self.page + 1));
                    ui.separator();

                    ui.label("Label");
                    let r = ui.text_edit_singleline(&mut self.editor.label);
                    if r.changed() { self.editor.dirty = true; }

                    ui.add_space(6.0);
                    ui.horizontal(|ui| {
                        ui.label("Icon");
                        if ui.small_button("📁 Open icons folder").clicked() {
                            self.open_images_dir();
                        }
                    });
                    ui.label(egui::RichText::new("Paste image path here (PNG/JPG/BMP)").size(10.0).weak());
                    let icon_r = ui.add(
                        egui::TextEdit::singleline(&mut self.editor.icon_path)
                            .hint_text("/path/to/image.png")
                            .desired_width(f32::INFINITY),
                    );
                    if icon_r.changed() {
                        self.editor.dirty = true;
                        self.reload_icon_texture(ctx);
                    }
                    if icon_r.lost_focus() {
                        self.reload_icon_texture(ctx);
                    }
                    // Preview thumbnail
                    if let Some(tex) = &self.icon_texture {
                        ui.add(egui::Image::new(tex).max_size(egui::vec2(72.0, 72.0)).rounding(4.0));
                    } else if !self.editor.icon_path.is_empty() {
                        ui.label(egui::RichText::new("⚠ Cannot load image").color(egui::Color32::YELLOW).size(11.0));
                    }

                    ui.add_space(6.0);
                    ui.label("Action (Lua)");
                    ui.add_space(2.0);
                    let r = ui.add(
                        egui::TextEdit::multiline(&mut self.editor.script)
                            .code_editor()
                            .desired_rows(8)
                            .desired_width(f32::INFINITY)
                            .hint_text("-- Lua code runs on button press\npc.run(\"firefox\")"),
                    );
                    if r.changed() { self.editor.dirty = true; }

                    ui.add_space(10.0);
                    ui.horizontal(|ui| {
                        let save_btn = egui::Button::new(
                            egui::RichText::new("💾 Save").strong()
                        );
                        if ui.add(save_btn).clicked() {
                            self.save_button(ctx);
                        }
                        if ui.button("Close").clicked() {
                            self.selected = None;
                        }
                    });
                });
        }

        // ── Main panel: device grid ───────────────────────────────────────────
        egui::CentralPanel::default().show(ctx, |ui| {
            if self.devices.is_empty() {
                ui.centered_and_justified(|ui| {
                    ui.heading("No device connected");
                });
                return;
            }

            egui::ScrollArea::vertical().show(ui, |ui| {
                let ids: Vec<String> = self.devices.keys().cloned().collect();
                for id in ids {
                    let device = self.devices[&id].clone();
                    ui.heading(&device.name);
                    match &device.kind {
                        DeviceKind::Ampligame => { ui.label("FIFINE Ampligame D6"); }
                        DeviceKind::HidKeyboard { event_path } => {
                            ui.label(format!("HID Keyboard  |  {event_path}"));
                        }
                        DeviceKind::Elgato => { ui.label("Elgato Stream Deck"); }
                    }
                    ui.add_space(4.0);
                    let page_count = self.profile.pages.len();
                    ui.horizontal(|ui| {
                        if ui.add_enabled(page_count > 1, egui::Button::new("◀")).clicked() {
                            let p = self.profile.step_page(self.page, PageNav::Prev);
                            self.go_to_page(p);
                        }
                        ui.label(egui::RichText::new(format!("Page {} / {page_count}", self.page + 1)).strong());
                        if ui.add_enabled(page_count > 1, egui::Button::new("▶")).clicked() {
                            let p = self.profile.step_page(self.page, PageNav::Next);
                            self.go_to_page(p);
                        }
                        ui.separator();
                        if ui.button("➕ Add page").clicked() {
                            self.add_page();
                        }
                        if ui.add_enabled(page_count > 1, egui::Button::new("🗑 Delete page")).clicked() {
                            self.delete_page();
                        }
                        ui.separator();
                        if ui.checkbox(&mut self.profile.circular, "Circular").changed() {
                            self.persist();
                        }
                        ui.separator();
                        if ui.button("📤 Push page to device").clicked() {
                            self.go_to_page(self.page);
                            self.push_log("Pushed page to device.".into());
                        }
                    });
                    ui.add_space(6.0);

                    egui::Grid::new(&id)
                        .num_columns(device.columns as usize)
                        .spacing([4.0, 4.0])
                        .show(ui, |ui| {
                            for row in 0..device.rows {
                                for col in 0..device.columns {
                                    let pos = row * device.columns + col;
                                    let cell_size = egui::vec2(72.0, 72.0);

                                    if let Some(nav) = self.profile.nav_at(pos) {
                                        let text = match nav { PageNav::Prev => "◀\nprev", PageNav::Next => "▶\nnext" };
                                        let btn = egui::Button::new(egui::RichText::new(text).size(13.0).strong())
                                            .min_size(cell_size);
                                        if ui.add(btn).on_hover_text("Page navigation (reserved)").clicked() {
                                            let p = self.profile.step_page(self.page, nav);
                                            self.go_to_page(p);
                                        }
                                        continue;
                                    }

                                    let is_selected = self.selected.as_ref()
                                        .map(|(d, p)| d == &id && *p == pos as u16)
                                        .unwrap_or(false);

                                    let btn_cfg = self.profile.button(self.page, pos);
                                    let top_line = btn_cfg
                                        .and_then(|b| b.label.as_deref())
                                        .unwrap_or("");
                                    let has_script = btn_cfg
                                        .and_then(|b| b.on_press.as_deref())
                                        .map(|s| !s.is_empty())
                                        .unwrap_or(false);
                                    let has_icon = btn_cfg
                                        .and_then(|b| b.icon.as_ref())
                                        .is_some();

                                    // Build button label
                                    let cell_text = if top_line.is_empty() {
                                        format!("{pos}")
                                    } else {
                                        format!("{top_line}\n({pos})")
                                    };

                                    // Status indicators
                                    let mut indicators = String::new();
                                    if has_script { indicators.push('⚡'); }
                                    if has_icon   { indicators.push('🖼'); }

                                    let full_text = if indicators.is_empty() {
                                        cell_text
                                    } else {
                                        format!("{cell_text}\n{indicators}")
                                    };

                                    let tex_key = (id.clone(), pos as u8);

                                    let clicked = if let Some(tex) = self.grid_textures.get(&tex_key) {
                                        // Show icon image as the button face
                                        let img = egui::Image::new(tex)
                                            .fit_to_exact_size(cell_size)
                                            .rounding(4.0);
                                        let btn = egui::ImageButton::new(img)
                                            .selected(is_selected);
                                        ui.add(btn).clicked()
                                    } else {
                                        // No icon — show text label
                                        let btn = egui::Button::new(
                                            egui::RichText::new(full_text).size(11.0).monospace(),
                                        )
                                        .min_size(cell_size)
                                        .selected(is_selected);
                                        ui.add(btn).clicked()
                                    };

                                    if clicked {
                                        self.select_button(ctx, &id, pos as u16);
                                    }
                                }
                                ui.end_row();
                            }
                        });

                    ui.add_space(6.0);
                    ui.label(egui::RichText::new("Click a button to edit it").size(11.0).weak());
                    ui.add_space(16.0);
                }
            });
        });

        ctx.request_repaint_after(std::time::Duration::from_millis(100));
    }
}

// ── Entry point ───────────────────────────────────────────────────────────────

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    env_logger::init();

    // Ensure config dirs exist
    let _ = std::fs::create_dir_all(config_dir());
    let _ = std::fs::create_dir_all(profiles_dir());

    let core = Arc::new(Core::start().await?);

    // Wait up to 3s for initial device discovery
    for _ in 0..30 {
        tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;
        if !core.current_devices().await.is_empty() { break; }
    }
    let initial_devices = core.current_devices().await;

    eframe::run_native(
        "RustDeck",
        eframe::NativeOptions {
            viewport: egui::ViewportBuilder::default()
                .with_inner_size([820.0, 600.0])
                .with_title("RustDeck"),
            ..Default::default()
        },
        Box::new(move |_cc| Ok(Box::new(App::new(core, initial_devices)))),
    )
    .map_err(|e| anyhow::anyhow!("eframe: {e}"))
}
