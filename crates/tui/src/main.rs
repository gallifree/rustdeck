use std::collections::HashMap;
use std::io;
use std::time::Duration;

use crossterm::{
    event::{self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyEventKind},
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use ratatui::{prelude::*, widgets::*};
use tokio::sync::broadcast;

use rustdeck_core::{
    Core,
    event::{DeviceInfo, Event as CoreEvent},
};

struct App {
    devices: HashMap<String, DeviceInfo>,
    log: Vec<String>,
    event_rx: broadcast::Receiver<CoreEvent>,
}

impl App {
    fn new(core: &Core) -> Self {
        Self {
            devices: HashMap::new(),
            log: Vec::new(),
            event_rx: core.subscribe(),
        }
    }

    fn drain_events(&mut self) {
        while let Ok(ev) = self.event_rx.try_recv() {
            match ev {
                CoreEvent::DeviceConnected(info) => {
                    self.push_log(format!("Connected: {}", info.name));
                    self.devices.insert(info.id.clone(), info);
                }
                CoreEvent::DeviceDisconnected(id) => {
                    self.push_log(format!("Disconnected: {id}"));
                    self.devices.remove(&id);
                }
                CoreEvent::ButtonPress { device: _, position } => {
                    self.push_log(format!("Button {position} ↓"));
                }
                CoreEvent::LuaError { context, error } => {
                    self.push_log(format!("Lua [{context}]: {error}"));
                }
                _ => {}
            }
        }
    }

    fn push_log(&mut self, msg: String) {
        self.log.push(msg);
        if self.log.len() > 100 {
            self.log.remove(0);
        }
    }
}

fn draw(f: &mut Frame, app: &App) {
    let [deck_area, log_area] = Layout::horizontal([
        Constraint::Percentage(65),
        Constraint::Percentage(35),
    ])
    .areas(f.area());

    // — Deck panel —
    let deck_block = Block::bordered().title(" Stream Deck ");
    let inner = deck_block.inner(deck_area);
    f.render_widget(deck_block, deck_area);

    if let Some(device) = app.devices.values().next() {
        let cols = device.columns as u16;
        let rows = device.rows as u16;
        let cell_w = (inner.width / cols).max(1);
        let cell_h = (inner.height / rows).max(1);

        for row in 0..rows {
            for col in 0..cols {
                let pos = row * cols + col;
                let rect = Rect {
                    x: inner.x + col * cell_w,
                    y: inner.y + row * cell_h,
                    width: cell_w.saturating_sub(1),
                    height: cell_h.saturating_sub(1),
                };
                let btn = Paragraph::new(format!(" {pos:2} "))
                    .alignment(Alignment::Center)
                    .block(Block::bordered().style(Style::default().bg(Color::DarkGray)));
                f.render_widget(btn, rect);
            }
        }
    } else {
        f.render_widget(
            Paragraph::new("No device connected")
                .alignment(Alignment::Center),
            inner,
        );
    }

    // — Log panel —
    let log_items: Vec<ListItem> = app
        .log
        .iter()
        .rev()
        .take(log_area.height as usize)
        .map(|l| ListItem::new(l.as_str()))
        .collect();

    f.render_widget(
        List::new(log_items).block(Block::bordered().title(" Log ")),
        log_area,
    );
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    env_logger::init();
    let core = Core::start().await?;
    let mut app = App::new(&core);

    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;
    let mut terminal = Terminal::new(CrosstermBackend::new(stdout))?;

    let result = run_loop(&mut terminal, &mut app).await;

    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen, DisableMouseCapture)?;
    terminal.show_cursor()?;

    result
}

async fn run_loop(terminal: &mut Terminal<CrosstermBackend<io::Stdout>>, app: &mut App) -> anyhow::Result<()> {
    loop {
        app.drain_events();
        terminal.draw(|f| draw(f, app))?;

        if event::poll(Duration::from_millis(50))? {
            if let Event::Key(key) = event::read()? {
                if key.kind == KeyEventKind::Press {
                    match key.code {
                        KeyCode::Char('q') | KeyCode::Esc => return Ok(()),
                        _ => {}
                    }
                }
            }
        }
    }
}
