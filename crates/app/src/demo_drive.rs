//! Debug builds only, and only for recording the giverny#530 demo: a window
//! driven from a command file, so a real window can be filmed without
//! synthetic input on the shared display. Not part of any PR.
//!
//! `GIVERNY_DEBUG_CMD=<file>`: when the file shows up it is taken, one
//! command per line, each carried out as the input it stands for:
//!
//! * `type <text>`: written to the active tab's terminal as typed
//!   (`\r` Enter, `\e` Esc);
//! * `text <text>`: the text as the keyboard types it into the window;
//! * `key <enter|shift-enter|up|down|esc|backspace|tab>`: that key;
//! * `move <x> <y> [ms]`: the pointer glides there and stays (a hover);
//! * `click <x> <y> [ms]`, `rclick <x> <y> [ms]`: glide there, then a left
//!   or right click;
//! * `wheel <lines> <x> <y>`: the wheel turned over a point, up when positive;
//! * `select <n>`: the n-th tab, from 0;
//! * `settings [<section title>]`: the settings screen on that section, or
//!   closed;
//! * `quit`.
//!
//! The pointer the commands move is painted as an arrow, with a ring on each
//! click, since the window's own pointer is not in a capture.

use std::sync::Mutex;
use std::time::{Duration, Instant};

use eframe::egui;
use egui::{Color32, Pos2, Stroke};

use crate::{Action, App};

/// Input queued by the commands, one frame's worth per entry.
static INPUT: Mutex<Vec<Vec<egui::Event>>> = Mutex::new(Vec::new());

/// Where the driven pointer is, and the last click (where, when).
static POINTER: Mutex<(Option<Pos2>, Option<(Pos2, Instant)>)> = Mutex::new((None, None));

fn feed(ctx: &egui::Context, events: Vec<egui::Event>) {
    if let Ok(mut q) = INPUT.lock() {
        q.push(events);
    }
    ctx.request_repaint();
}

fn key(ctx: &egui::Context, key: egui::Key, modifiers: egui::Modifiers) {
    let ev = |pressed| egui::Event::Key {
        key,
        physical_key: None,
        pressed,
        repeat: false,
        modifiers,
    };
    feed(ctx, vec![ev(true)]);
    feed(ctx, vec![ev(false)]);
}

/// The pointer from where it is to `to`, a frame per step, over `ms`.
fn glide(ctx: &egui::Context, to: Pos2, ms: u64) {
    let from = POINTER.lock().ok().and_then(|p| p.0).unwrap_or(to);
    let steps = (ms / 16).max(1);
    for k in 1..=steps {
        let t = k as f32 / steps as f32;
        // Ease in and out, as a hand moves.
        let t = t * t * (3.0 - 2.0 * t);
        feed(ctx, vec![egui::Event::PointerMoved(from + (to - from) * t)]);
    }
}

fn numbers(arg: &str) -> Vec<f32> {
    arg.split_whitespace()
        .filter_map(|v| v.parse().ok())
        .collect()
}

/// Take the command file, if it is there, and queue what it says.
pub fn take(app: &mut App, ctx: &egui::Context) {
    let Ok(file) = std::env::var("GIVERNY_DEBUG_CMD") else {
        return;
    };
    ctx.request_repaint_after(Duration::from_millis(100));
    let Ok(spec) = std::fs::read_to_string(&file) else {
        return;
    };
    let _ = std::fs::remove_file(&file);
    for line in spec.lines().map(str::trim).filter(|l| !l.is_empty()) {
        let (cmd, arg) = line.split_once(' ').unwrap_or((line, ""));
        tracing::info!("demo cmd: {line}");
        match cmd {
            "type" => {
                let session = app
                    .ws
                    .active
                    .and_then(|t| app.rt.get(&t))
                    .and_then(|rt| rt.session.as_ref());
                if let Some(session) = session {
                    session.note_user_input();
                    let text = arg.replace("\\r", "\r").replace("\\e", "\x1b");
                    session.write(text.into_bytes());
                }
            }
            "text" => feed(ctx, vec![egui::Event::Text(arg.to_string())]),
            "key" => {
                let none = egui::Modifiers::NONE;
                match arg {
                    "enter" => key(ctx, egui::Key::Enter, none),
                    "shift-enter" => key(ctx, egui::Key::Enter, egui::Modifiers::SHIFT),
                    "up" => key(ctx, egui::Key::ArrowUp, none),
                    "down" => key(ctx, egui::Key::ArrowDown, none),
                    "esc" => key(ctx, egui::Key::Escape, none),
                    "backspace" => key(ctx, egui::Key::Backspace, none),
                    "tab" => key(ctx, egui::Key::Tab, none),
                    _ => tracing::warn!("demo cmd: no key {arg}"),
                }
            }
            "move" | "click" | "rclick" => {
                let n = numbers(arg);
                let (to, ms) = match n.as_slice() {
                    [x, y] => (Pos2::new(*x, *y), 450),
                    [x, y, ms] => (Pos2::new(*x, *y), *ms as u64),
                    _ => {
                        tracing::warn!("demo cmd: no such point {arg}");
                        continue;
                    }
                };
                glide(ctx, to, ms);
                if cmd != "move" {
                    let button = if cmd == "click" {
                        egui::PointerButton::Primary
                    } else {
                        egui::PointerButton::Secondary
                    };
                    let ev = |pressed| egui::Event::PointerButton {
                        pos: to,
                        button,
                        pressed,
                        modifiers: egui::Modifiers::NONE,
                    };
                    feed(ctx, vec![ev(true)]);
                    feed(ctx, vec![ev(false)]);
                }
            }
            "wheel" => {
                let n = numbers(arg);
                let [lines, x, y] = n.as_slice() else {
                    tracing::warn!("demo cmd: no such wheel {arg}");
                    continue;
                };
                let pos = Pos2::new(*x, *y);
                glide(ctx, pos, 250);
                feed(
                    ctx,
                    vec![egui::Event::MouseWheel {
                        unit: egui::MouseWheelUnit::Line,
                        delta: egui::vec2(0.0, *lines),
                        phase: egui::TouchPhase::Move,
                        modifiers: egui::Modifiers::NONE,
                    }],
                );
            }
            "select" => {
                let tab = arg.parse::<usize>().ok().and_then(|n| app.ws.tabs.get(n));
                match tab.map(|t| t.id) {
                    Some(id) => app.apply(ctx, Action::Select(id)),
                    None => tracing::warn!("demo cmd: no tab {arg}"),
                }
            }
            "settings" => {
                app.settings = giverny_core::settings::Section::ALL
                    .iter()
                    .find(|s| s.title() == arg)
                    .map(|&section| crate::settings_ui::SettingsState {
                        section,
                        search_focus: false,
                        ..Default::default()
                    });
                app.focus_terminal = app.settings.is_none();
            }
            "quit" => ctx.send_viewport_cmd(egui::ViewportCommand::Close),
            _ => tracing::warn!("demo cmd: unknown {line}"),
        }
    }
}

/// One frame's queued input into the frame, as the window's own would come.
pub fn raw_input(app: &mut App, ctx: &egui::Context, raw_input: &mut egui::RawInput) {
    let Ok(mut q) = INPUT.lock() else {
        return;
    };
    if q.is_empty() {
        return;
    }
    let events = q.remove(0);
    let keys = events
        .iter()
        .any(|e| matches!(e, egui::Event::Key { .. } | egui::Event::Text(_)));
    if let Ok(mut p) = POINTER.lock() {
        for e in &events {
            match e {
                egui::Event::PointerMoved(pos) => p.0 = Some(*pos),
                egui::Event::PointerButton {
                    pos, pressed: true, ..
                } => p.1 = Some((*pos, Instant::now())),
                _ => {}
            }
        }
    }
    raw_input.events.extend(events);
    // As if the window had the keyboard: egui drops a widget's focus while
    // it has not.
    raw_input.focused = true;
    if keys && app.settings.is_none() {
        app.focus_terminal = true;
    }
    ctx.request_repaint();
}

/// The driven pointer, drawn over everything: an arrow, and a ring that
/// widens and fades for half a second after a click.
pub fn paint_pointer(ctx: &egui::Context) {
    if std::env::var_os("GIVERNY_DEBUG_CMD").is_none() {
        return;
    }
    let Ok(p) = POINTER.lock() else {
        return;
    };
    let painter = ctx.layer_painter(egui::LayerId::new(
        egui::Order::Debug,
        egui::Id::new("demo-pointer"),
    ));
    if let Some((at, when)) = p.1 {
        let t = when.elapsed().as_secs_f32() / 0.5;
        if t < 1.0 {
            painter.circle_stroke(
                at,
                6.0 + 16.0 * t,
                Stroke::new(3.0, Color32::from_rgba_unmultiplied(255, 210, 90, (230.0 * (1.0 - t)) as u8)),
            );
            ctx.request_repaint();
        }
    }
    if let Some(at) = p.0 {
        let v = |x: f32, y: f32| at + egui::vec2(x, y) * 1.2;
        // The tail, then the head over it.
        painter.line_segment([v(4.0, 11.0), v(8.5, 20.0)], Stroke::new(4.6, Color32::BLACK));
        painter.line_segment([v(4.0, 11.0), v(8.5, 20.0)], Stroke::new(2.6, Color32::WHITE));
        painter.add(egui::Shape::convex_polygon(
            vec![v(0.0, 0.0), v(0.0, 16.0), v(11.5, 11.5)],
            Color32::WHITE,
            Stroke::new(1.2, Color32::BLACK),
        ));
    }
}
