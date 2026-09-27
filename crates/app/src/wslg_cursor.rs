//! WSLg: one cursor theme, at the size Windows draws its own (#100).
//!
//! Under WSLg the window is an X11 one (see `wslg::avoid_wayland`) on a
//! display that says it is at scale 1, so libXcursor sizes cursors from the
//! screen's height (1800 / 48 = 37 on a 2880x1800 panel) rather than from
//! the display's scale. And two cursors on the window were never Giverny's
//! at all:
//!
//! - winit starts every window on the default cursor without telling the X
//!   server, so until egui asks for some other cursor the window shows its
//!   parent's;
//! - Weston's X window manager keeps a margin round an undecorated window
//!   (see `titlebar::Maximize`), and WSLg hands the margin to Windows as part
//!   of the window. The pointer crosses it on its way off any edge.
//!
//! Both of those show the cursor Weston put on the root window: a grey arrow
//! from another theme, 24 px where Windows' own arrow is drawn in 48. So a
//! pointer that crossed a resize edge came back tiny and grey, and a fresh
//! window started that way.
//!
//! Here the display's cursor size is set from the display scale (unless the
//! user chose one), and the theme's default cursor is put on the window and
//! on the frame Weston wrapped it in. The theme is whatever libXcursor
//! finds: `XCURSOR_THEME` when set, the system's default theme otherwise.
//!
//! Nothing is set in the environment, so the shells in the tabs, and the X
//! programs started from them, get the same cursors they would get from any
//! other terminal.

use raw_window_handle::{HasDisplayHandle, HasWindowHandle, RawDisplayHandle, RawWindowHandle};
use std::ffi::{CStr, c_ulong};
use std::time::{Duration, Instant};
use x11_dl::xcursor::Xcursor;
use x11_dl::xlib::{Display, Xlib};

/// A cursor theme's size at 100 %: GNOME's and KDE's default.
const BASE_SIZE: f32 = 24.0;

/// The scale Windows draws its own cursors at on a display at `scale`.
///
/// Windows does not follow the display all the way: at 175 % its arrow is
/// 18x27 px in a 48-px cursor, ×1.5 (measured with `GetIconInfo`). Rounding
/// down to a half step reproduces that, and at 24 × 1.5 = 36 libXcursor
/// picks a theme's 32-px cursor, whose arrow (Adwaita's is 17x28) is the
/// same size as Windows'. The full 1.75 (42 px, Adwaita's 48) looked too big
/// next to every other window on the screen.
fn windows_cursor_scale(scale: f32) -> f32 {
    ((scale * 2.0).floor() / 2.0).max(1.0)
}

/// How often to look for a new frame. Weston makes one each time the window
/// is mapped; a query costs one round trip to the X server.
const RECHECK: Duration = Duration::from_secs(1);

/// The cursor size to ask libXcursor for at this display scale, or `None`
/// where the user has chosen one: `XCURSOR_SIZE` (read the way libXcursor
/// reads it, so `0` or junk is no choice) or `Xcursor.size` in the X
/// resources.
fn cursor_size(env: Option<&str>, resources: Option<&str>, scale: f32) -> Option<i32> {
    let chosen = |v: &str| v.trim().parse::<i32>().is_ok_and(|n| n > 0);
    if env.is_some_and(chosen) {
        return None;
    }
    let in_resources = resources.is_some_and(|db| {
        db.lines().any(|line| {
            line.split_once(':').is_some_and(|(key, value)| {
                matches!(key.trim(), "Xcursor.size" | "Xcursor*size") && chosen(value)
            })
        })
    });
    if in_resources {
        return None;
    }
    Some((BASE_SIZE * windows_cursor_scale(scale)).round() as i32)
}

/// Giverny's cursors on WSLg's X server.
pub struct Cursors {
    xlib: Xlib,
    display: *mut Display,
    window: c_ulong,
    /// The theme's default cursor, for the window and its frame.
    arrow: c_ulong,
    /// The frame it was last put on.
    frame: c_ulong,
    checked: Instant,
}

impl Cursors {
    /// Size the display's cursors for `scale` (when known) and put the
    /// theme's default cursor on the window. Must run before winit loads a
    /// cursor, which it first does after the first frame: winit keeps each
    /// cursor it loads.
    pub fn new(
        handles: &(impl HasWindowHandle + HasDisplayHandle),
        scale: Option<f32>,
    ) -> Option<Self> {
        Self::try_new(handles, scale)
            .inspect_err(|why| tracing::warn!("WSLg: cursors left as they were: {why}"))
            .ok()
    }

    fn try_new(
        handles: &(impl HasWindowHandle + HasDisplayHandle),
        scale: Option<f32>,
    ) -> Result<Self, String> {
        let display = handles.display_handle().map_err(|e| e.to_string())?;
        let RawDisplayHandle::Xlib(display) = display.as_raw() else {
            return Err(format!("not an Xlib display: {:?}", display.as_raw()));
        };
        let window = handles.window_handle().map_err(|e| e.to_string())?;
        let RawWindowHandle::Xlib(window) = window.as_raw() else {
            return Err(format!("not an Xlib window: {:?}", window.as_raw()));
        };
        let display = display
            .display
            .ok_or("no Xlib display pointer")?
            .as_ptr()
            .cast::<Display>();
        let xlib = Xlib::open().map_err(|e| e.to_string())?;
        let xcursor = Xcursor::open().map_err(|e| e.to_string())?;
        // SAFETY: `display` is winit's open connection, which outlives the
        // app; everything here runs on the event-loop thread winit uses it
        // from.
        unsafe {
            if let Some(scale) = scale {
                let resources = (xlib.XResourceManagerString)(display);
                let resources = (!resources.is_null())
                    .then(|| CStr::from_ptr(resources).to_string_lossy().into_owned());
                let env = std::env::var("XCURSOR_SIZE").ok();
                match cursor_size(env.as_deref(), resources.as_deref(), scale) {
                    Some(size) => {
                        (xcursor.XcursorSetDefaultSize)(display, size);
                        tracing::info!("WSLg: cursors at {size} px (display scale {scale})");
                    }
                    None => tracing::info!("WSLg: cursor size left to the user's setting"),
                }
            }
            let arrow = [c"default", c"left_ptr"]
                .into_iter()
                .map(|name| (xcursor.XcursorLibraryLoadCursor)(display, name.as_ptr()))
                .find(|&c| c != 0)
                .ok_or("the cursor theme has no default cursor")?;
            (xlib.XDefineCursor)(display, window.window, arrow);
            (xlib.XFlush)(display);
            let mut cursors = Cursors {
                xlib,
                display,
                window: window.window,
                arrow,
                frame: 0,
                checked: Instant::now(),
            };
            cursors.dress_frame();
            Ok(cursors)
        }
    }

    /// Put the default cursor on the window's frame, if it has a new one.
    /// Cheap enough to call every frame: it looks once a second.
    pub fn keep(&mut self) {
        if self.checked.elapsed() >= RECHECK {
            self.checked = Instant::now();
            self.dress_frame();
        }
    }

    fn dress_frame(&mut self) {
        let (mut root, mut parent, mut children, mut n) = (0, 0, std::ptr::null_mut(), 0);
        // SAFETY: as in `new`.
        unsafe {
            let ok = (self.xlib.XQueryTree)(
                self.display,
                self.window,
                &mut root,
                &mut parent,
                &mut children,
                &mut n,
            );
            if !children.is_null() {
                (self.xlib.XFree)(children.cast());
            }
            // Not reparented yet (the root is everyone's), or no answer.
            if ok == 0 || parent == 0 || parent == root || parent == self.frame {
                return;
            }
            (self.xlib.XDefineCursor)(self.display, parent, self.arrow);
            (self.xlib.XFlush)(self.display);
        }
        self.frame = parent;
        tracing::debug!("WSLg: default cursor put on the frame, {parent:#x}");
    }
}

#[cfg(test)]
mod tests {
    use super::cursor_size;

    #[test]
    fn scales_like_windows_does() {
        // 175 %: Windows' cursors are 48 px (×1.5), not 56.
        assert_eq!(cursor_size(None, None, 1.75), Some(36));
        assert_eq!(cursor_size(None, None, 1.5), Some(36));
        assert_eq!(cursor_size(None, None, 1.25), Some(24));
        assert_eq!(cursor_size(None, None, 1.0), Some(24));
        assert_eq!(cursor_size(None, None, 2.0), Some(48));
        assert_eq!(cursor_size(None, None, 2.25), Some(48));
    }

    #[test]
    fn a_users_size_wins() {
        assert_eq!(cursor_size(Some("32"), None, 1.75), None);
        assert_eq!(
            cursor_size(None, Some("Xft.dpi:\t168\nXcursor.size:\t48\n"), 1.75),
            None
        );
        assert_eq!(cursor_size(None, Some("Xcursor*size: 30"), 1.75), None);
    }

    #[test]
    fn no_size_is_no_choice() {
        // libXcursor reads XCURSOR_SIZE with atoi: 0 and junk mean unset.
        assert_eq!(cursor_size(Some("0"), None, 1.75), Some(36));
        assert_eq!(cursor_size(Some("big"), None, 1.75), Some(36));
        assert_eq!(cursor_size(Some(""), Some("Xft.dpi: 168"), 1.75), Some(36));
        assert_eq!(
            cursor_size(None, Some("Xcursor.theme: Adwaita"), 1.75),
            Some(36)
        );
    }
}
