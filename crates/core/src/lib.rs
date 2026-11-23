pub mod ampligame;
pub mod config;
pub mod device;
pub mod event;
pub mod lua;
pub mod render;

use tokio::sync::{Mutex, RwLock, broadcast, mpsc};
use anyhow::Result;
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use config::Profile;
use event::{Command, DeviceInfo, Event};
use lua::LuaEngine;

struct LuaJob {
    code: String,
    context: String,
}

/// State shared by the core's background tasks.
#[derive(Clone)]
struct Pages {
    event_tx: broadcast::Sender<Event>,
    devices:  Arc<RwLock<HashMap<String, DeviceInfo>>>,
    profile:  Arc<RwLock<Profile>>,
    current:  Arc<AtomicUsize>,
    /// Serializes redraws so two pages never interleave on the HID link.
    draw:     Arc<Mutex<()>>,
}

impl Pages {
    async fn show(&self, page: usize) {
        let profile = self.profile.read().await.clone();
        let page = page.min(profile.pages.len().saturating_sub(1));
        self.current.store(page, Ordering::SeqCst);
        log::info!("show page {} / {}", page + 1, profile.pages.len());
        let _ = self.event_tx.send(Event::PageChanged(page));
        let devs: Vec<DeviceInfo> = self.devices.read().await.values().cloned().collect();
        let _guard = self.draw.lock().await;
        for d in &devs {
            render::render_page(d, &profile, page).await;
        }
    }

    async fn draw_device(&self, dev: &DeviceInfo) {
        let profile = self.profile.read().await.clone();
        let _guard = self.draw.lock().await;
        render::render_page(dev, &profile, self.current.load(Ordering::SeqCst)).await;
    }
}

pub struct Core {
    event_tx:   broadcast::Sender<Event>,
    command_tx: mpsc::Sender<Command>,
    /// Live device snapshot — always up to date
    devices:    Arc<RwLock<HashMap<String, DeviceInfo>>>,
    current_page: Arc<AtomicUsize>,
}

impl Core {
    pub async fn start() -> Result<Self> {
        let (event_tx, _) = broadcast::channel::<Event>(256);
        let (cmd_tx, mut cmd_rx) = mpsc::channel::<Command>(64);
        let devices: Arc<RwLock<HashMap<String, DeviceInfo>>> =
            Arc::new(RwLock::new(HashMap::new()));

        // — Device discovery loop (Elgato + generic HID keyboard) —
        {
            let tx = event_tx.clone();
            tokio::spawn(async move {
                loop {
                    device::discover(tx.clone()).await;
                    tokio::time::sleep(tokio::time::Duration::from_secs(2)).await;
                }
            });
        }

        // — Ampligame D6 discovery + hot-plug watcher (runs for the lifetime of the app) —
        {
            let tx = event_tx.clone();
            tokio::spawn(async move {
                ampligame::discover_and_watch(tx).await;
            });
        }

        // — Profile + current page —
        let pages = Pages {
            event_tx: event_tx.clone(),
            devices:  devices.clone(),
            profile:  Arc::new(RwLock::new(
                Profile::load(&config::default_profile_path()).unwrap_or_default(),
            )),
            current:  Arc::new(AtomicUsize::new(0)),
            draw:     Arc::new(Mutex::new(())),
        };

        // — Mirror events into the devices snapshot; draw current page on connect —
        {
            let mut rx = event_tx.subscribe();
            let devices = devices.clone();
            let pages = pages.clone();
            tokio::spawn(async move {
                while let Ok(ev) = rx.recv().await {
                    match ev {
                        Event::DeviceConnected(info) => {
                            devices.write().await.insert(info.id.clone(), info.clone());
                            let pages = pages.clone();
                            tokio::spawn(async move { pages.draw_device(&info).await });
                        }
                        Event::DeviceDisconnected(ref id) => {
                            devices.write().await.remove(id);
                        }
                        _ => {}
                    }
                }
            });
        }

        // — Dedicated OS thread for Lua (Lua is !Sync) —
        let (lua_tx, lua_rx) = std::sync::mpsc::channel::<LuaJob>();
        {
            let event_tx2 = event_tx.clone();
            std::thread::spawn(move || {
                let engine = match LuaEngine::new() {
                    Ok(e) => e,
                    Err(e) => { log::error!("Lua init failed: {e}"); return; }
                };
                while let Ok(job) = lua_rx.recv() {
                    if let Err(e) = engine.exec(&job.code) {
                        let _ = event_tx2.send(Event::LuaError {
                            context: job.context,
                            error: e.to_string(),
                        });
                    }
                }
            });
        }

        // — Event handler: button press → page navigation or Lua —
        {
            let mut rx = event_tx.subscribe();
            let pages = pages.clone();
            tokio::spawn(async move {
                while let Ok(ev) = rx.recv().await {
                    if let Event::ButtonPress { ref device, position } = ev {
                        let pos = position as u8;
                        let (target, script) = {
                            let p = pages.profile.read().await;
                            let cur = pages.current.load(Ordering::SeqCst);
                            match p.nav_at(pos) {
                                Some(nav) => (Some(p.step_page(cur, nav)), None),
                                None => (None, p.button(cur, pos).and_then(|b| b.on_press.clone())),
                            }
                        };
                        log::info!("press pos={pos} nav_target={target:?}");
                        if let Some(page) = target {
                            pages.show(page).await;
                        } else if let Some(raw) = script {
                            match config::resolve_script(&raw) {
                                Ok(code) => { let _ = lua_tx.send(LuaJob { code, context: format!("{device}:{position}") }); }
                                Err(e)   => log::warn!("Failed to resolve script: {e}"),
                            }
                        }
                    }
                }
            });
        }

        // — Command handler —
        let current_page = pages.current.clone();
        {
            tokio::spawn(async move {
                while let Some(cmd) = cmd_rx.recv().await {
                    match cmd {
                        Command::SetBrightness { ref device, brightness } => {
                            if device.starts_with("ampgd6") {
                                ampligame::set_brightness(device, brightness).await;
                            } else {
                                device::set_brightness(device, brightness).await;
                            }
                        }
                        Command::SetButtonImage { ref device, position, ref png } => {
                            let _guard = pages.draw.lock().await;
                            if let Ok(img) = image::load_from_memory(png) {
                                if device.starts_with("ampgd6") {
                                    ampligame::set_button_image(device, position, img).await;
                                } else {
                                    let _ = device::set_button_image(device, position, img).await;
                                }
                            }
                        }
                        Command::ClearButton { ref device, position } => {
                            let _ = device::clear_button(device, position).await;
                        }
                        Command::SwitchProfile(ref name) => {
                            let path = config::profiles_dir().join(format!("{name}.toml"));
                            if let Ok(p) = Profile::load(&path) {
                                *pages.profile.write().await = p;
                                pages.show(0).await;
                            }
                        }
                        Command::ReloadProfile { page } => {
                            match Profile::load(&config::default_profile_path()) {
                                Ok(p) => {
                                    *pages.profile.write().await = p;
                                    pages.show(page).await;
                                }
                                Err(e) => log::warn!("Profile reload failed: {e}"),
                            }
                        }
                        Command::ShowPage(page) => pages.show(page).await,
                        Command::Shutdown => break,
                    }
                }
            });
        }

        Ok(Self { event_tx, command_tx: cmd_tx, devices, current_page })
    }

    pub fn current_page(&self) -> usize {
        self.current_page.load(Ordering::SeqCst)
    }

    pub fn subscribe(&self) -> broadcast::Receiver<Event> {
        self.event_tx.subscribe()
    }

    pub fn commands(&self) -> mpsc::Sender<Command> {
        self.command_tx.clone()
    }

    /// Snapshot of currently connected devices — safe to call at any time.
    pub async fn current_devices(&self) -> HashMap<String, DeviceInfo> {
        self.devices.read().await.clone()
    }
}
