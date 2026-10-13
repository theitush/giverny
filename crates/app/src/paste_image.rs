//! Pasting a screenshot.
//!
//! Ctrl+V (Cmd+V) with an image and no text in the clipboard used to do
//! nothing: egui only ever pastes text, and Claude Code's own clipboard read
//! needs `xclip`/`wl-paste`, which a WSL distro rarely has. So Giverny reads
//! the image itself, writes it to a PNG of its own, and pastes the file's
//! path the way a file drop types one: Claude Code attaches an image path
//! pasted into its prompt (`[Image #1]`), and a shell gets the path.
//!
//! What a paste types, in order: the clipboard's text when it has some (and
//! files copied in Finder over the display names that come with them, see
//! `giverny_term::input::copied_files_text`); with no text, copied files'
//! paths; with neither, an image.
//!
//! Where the image comes from:
//! * the clipboard through `arboard` — X11 (or XWayland) on Linux,
//!   NSPasteboard on macOS;
//! * on WSL, when that has none, the Windows clipboard through interop.
//!   WSLg's clipboard bridge carries text only: a Win+Shift+S snip shows up
//!   in X as an empty `UTF8_STRING` and no image target at all.
//!
//! The read runs on a thread of its own: PowerShell takes the better part of
//! a second to start, and a frame must never wait on it.
//!
//! The PNGs live in a directory of this run's (`giverny-paste-<pid>` under
//! `$XDG_RUNTIME_DIR`, or the temp dir), removed when the app exits; files
//! a crashed run left behind go at the next start once they are a day old.

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use crossbeam_channel::{Receiver, Sender};
use giverny_core::tabs::TabId;

/// The prefix of every run's paste directory, which the start-up sweep
/// looks for.
const DIR_PREFIX: &str = "giverny-paste-";

/// Pasted images older than this are a dead run's, and swept at start.
const STALE_AFTER: Duration = Duration::from_secs(24 * 60 * 60);

/// The eight bytes every PNG starts with.
const PNG_MAGIC: &[u8] = b"\x89PNG\r\n\x1a\n";

/// The Windows clipboard's image, as base64 PNG on stdout: the `PNG` format
/// itself when the copier offered one (a Win+Shift+S snip does, alpha and
/// all), else the bitmap re-encoded. Nothing printed when there is no image.
const WINDOWS_CLIPBOARD_PS: &str = "\
Add-Type -AssemblyName System.Windows.Forms,System.Drawing; \
$d = [Windows.Forms.Clipboard]::GetDataObject(); \
$m = New-Object IO.MemoryStream; \
if ($d -and $d.GetDataPresent('PNG')) { $d.GetData('PNG').CopyTo($m) } \
elseif ([Windows.Forms.Clipboard]::ContainsImage()) { \
[Windows.Forms.Clipboard]::GetImage().Save($m, [Drawing.Imaging.ImageFormat]::Png) }; \
if ($m.Length -gt 0) { [Convert]::ToBase64String($m.ToArray()) }";

/// What a paste with no text found to type, and the tab it was pasted in:
/// an image's saved file, or copied files, as a drop of them would type.
pub struct Pasted {
    pub tab: TabId,
    pub text: String,
}

/// The app's side of image pastes: the run's directory, and the threads'
/// answers.
pub struct ImagePaste {
    /// This run's directory; created on the first paste.
    dir: PathBuf,
    tx: Sender<Pasted>,
    rx: Receiver<Pasted>,
}

impl ImagePaste {
    /// Sets up this run's directory name and sweeps what earlier runs left
    /// behind. Nothing is created until something is pasted.
    pub fn new() -> Self {
        let base = std::env::var_os("XDG_RUNTIME_DIR")
            .map(PathBuf::from)
            .filter(|p| p.is_dir())
            .unwrap_or_else(std::env::temp_dir);
        sweep(&base, SystemTime::now(), STALE_AFTER);
        let (tx, rx) = crossbeam_channel::unbounded();
        Self {
            dir: base.join(format!("{DIR_PREFIX}{}", std::process::id())),
            tx,
            rx,
        }
    }

    /// Look for copied files, else an image, in the clipboard, in the
    /// background, and when there is either have it pasted into `tab` (see
    /// [`Self::poll`]).
    pub fn request(&mut self, ctx: &egui::Context, tab: TabId) {
        let dir = self.dir.clone();
        let tx = self.tx.clone();
        let ctx = ctx.clone();
        let spawned = std::thread::Builder::new()
            .name("paste image".into())
            .spawn(move || {
                let send = |text: String| {
                    let _ = tx.send(Pasted { tab, text });
                    ctx.request_repaint();
                };
                // Files copied with no text beside them (Finder's always
                // come with their names, and are pasted as text is).
                if let Some(text) = giverny_term::input::copied_files_text() {
                    return send(text);
                }
                let Some(png) = clipboard_png().filter(|b| b.starts_with(PNG_MAGIC)) else {
                    tracing::info!("paste: no text, files or image in the clipboard");
                    return;
                };
                match save(&dir, &png, SystemTime::now()) {
                    Ok(path) => {
                        tracing::info!("paste: image saved to {}", path.display());
                        send(giverny_term::input::dropped_paths_text(&[path
                            .display()
                            .to_string()]));
                    }
                    Err(e) => tracing::warn!("paste: image not saved in {}: {e}", dir.display()),
                }
            });
        if let Err(e) = spawned {
            tracing::warn!("paste: no thread for the image read: {e}");
        }
    }

    /// What the threads found since the last call, oldest first.
    pub fn poll(&self) -> Vec<Pasted> {
        self.rx.try_iter().collect()
    }

    /// Remove this run's pasted images. Run at exit: by then Claude Code
    /// has long read every one it was given.
    pub fn clean_up(&self) {
        if self.dir.exists()
            && let Err(e) = std::fs::remove_dir_all(&self.dir)
        {
            tracing::debug!("paste: {} not removed: {e}", self.dir.display());
        }
    }
}

impl Drop for ImagePaste {
    fn drop(&mut self) {
        self.clean_up();
    }
}

/// The clipboard's image as PNG bytes, from wherever this platform keeps it.
fn clipboard_png() -> Option<Vec<u8>> {
    if let Some(png) = arboard_png() {
        return Some(png);
    }
    if is_wsl() {
        return windows_clipboard_png();
    }
    None
}

/// Running in WSL, where the Windows clipboard is a process away.
fn is_wsl() -> bool {
    std::env::var_os("WSL_DISTRO_NAME").is_some()
        || Path::new("/proc/sys/fs/binfmt_misc/WSLInterop").exists()
}

/// The clipboard's image through `arboard` (raw RGBA), encoded as PNG.
fn arboard_png() -> Option<Vec<u8>> {
    let image = arboard::Clipboard::new()
        .and_then(|mut c| c.get_image())
        .inspect_err(|e| tracing::debug!("paste: no clipboard image through arboard: {e}"))
        .ok()?;
    encode_png(image.width, image.height, &image.bytes)
        .inspect_err(|e| tracing::warn!("paste: clipboard image not encoded: {e}"))
        .ok()
}

/// RGBA pixels as a PNG file's bytes.
fn encode_png(width: usize, height: usize, rgba: &[u8]) -> Result<Vec<u8>, png::EncodingError> {
    let mut out = Vec::new();
    let mut encoder = png::Encoder::new(&mut out, width as u32, height as u32);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    let mut writer = encoder.write_header()?;
    writer.write_image_data(rgba)?;
    writer.finish()?;
    Ok(out)
}

/// WSL: the Windows clipboard's image, through PowerShell over interop.
/// Base64 on stdout rather than a file Windows writes: no path to translate,
/// and nothing of Windows' to clean up.
fn windows_clipboard_png() -> Option<Vec<u8>> {
    use base64::Engine;
    let ps = ["/mnt/c/Windows/System32/WindowsPowerShell/v1.0/powershell.exe"]
        .into_iter()
        .find(|p| Path::new(p).exists())
        .unwrap_or("powershell.exe");
    let out = std::process::Command::new(ps)
        // The clipboard is an OLE service: single-threaded apartment only.
        .args(["-NoProfile", "-NonInteractive", "-Sta", "-Command"])
        .arg(WINDOWS_CLIPBOARD_PS)
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .output()
        .inspect_err(|e| tracing::warn!("paste: {ps} did not run: {e}"))
        .ok()?;
    let text = String::from_utf8_lossy(&out.stdout);
    let text = text.trim();
    if text.is_empty() {
        return None;
    }
    base64::engine::general_purpose::STANDARD
        .decode(text)
        .inspect_err(|e| tracing::warn!("paste: Windows clipboard image unreadable: {e}"))
        .ok()
}

/// The file name for an image pasted at `now`: its local time to the
/// millisecond, so a directory listing reads in paste order.
fn file_name(now: SystemTime) -> String {
    let stamp = jiff::Timestamp::try_from(now)
        .unwrap_or(jiff::Timestamp::UNIX_EPOCH)
        .to_zoned(jiff::tz::TimeZone::system());
    format!(
        "paste-{}-{:03}.png",
        stamp.strftime("%Y%m%d-%H%M%S"),
        stamp.subsec_nanosecond() / 1_000_000
    )
}

/// Write the PNG into `dir` (made private on first use) under a fresh
/// name: two pastes in the same millisecond each get a file.
fn save(dir: &Path, png: &[u8], now: SystemTime) -> std::io::Result<PathBuf> {
    let mut builder = std::fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    {
        // Under a shared /tmp: a screenshot is nobody else's business.
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder.create(dir)?;
    let name = file_name(now);
    let stem = name.trim_end_matches(".png");
    for n in 1.. {
        let path = if n == 1 {
            dir.join(&name)
        } else {
            dir.join(format!("{stem}-{n}.png"))
        };
        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
        {
            Ok(mut file) => {
                std::io::Write::write_all(&mut file, png)?;
                return Ok(path);
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e),
        }
    }
    unreachable!("an unbounded range ends only by returning")
}

/// Remove pasted images older than `max_age` from every run's directory
/// under `base`, and the directories they leave empty. A run still going
/// keeps its directory as long as it holds anything younger.
fn sweep(base: &Path, now: SystemTime, max_age: Duration) {
    let Ok(entries) = std::fs::read_dir(base) else {
        return;
    };
    for entry in entries.flatten() {
        let is_ours = entry
            .file_name()
            .to_str()
            .is_some_and(|n| n.starts_with(DIR_PREFIX));
        if !is_ours || !entry.file_type().is_ok_and(|t| t.is_dir()) {
            continue;
        }
        let dir = entry.path();
        for file in std::fs::read_dir(&dir).into_iter().flatten().flatten() {
            let old = file
                .metadata()
                .and_then(|m| m.modified())
                .ok()
                .and_then(|t| now.duration_since(t).ok())
                .is_some_and(|age| age > max_age);
            if old {
                let _ = std::fs::remove_file(file.path());
            }
        }
        // Fails while anything is left in it, which is the point.
        let _ = std::fs::remove_dir(&dir);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A fresh scratch directory under the temp dir.
    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "giverny-test-{name}-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(SystemTime::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn names_are_pngs_by_time() {
        let at = SystemTime::UNIX_EPOCH + Duration::from_millis(1_760_000_000_123);
        let name = file_name(at);
        assert!(name.starts_with("paste-20"), "{name}");
        assert!(name.ends_with("-123.png"), "milliseconds last: {name}");
        // paste-YYYYMMDD-HHMMSS-mmm.png
        assert_eq!(name.len(), "paste-20251009-090000-123.png".len(), "{name}");
    }

    #[test]
    fn same_millisecond_pastes_get_their_own_files() {
        let base = scratch("save");
        let dir = base.join(format!("{DIR_PREFIX}1"));
        let now = SystemTime::now();
        let a = save(&dir, b"one", now).unwrap();
        let b = save(&dir, b"two", now).unwrap();
        assert_ne!(a, b);
        assert_eq!(std::fs::read(&a).unwrap(), b"one");
        assert_eq!(std::fs::read(&b).unwrap(), b"two");
        assert!(b.to_string_lossy().ends_with("-2.png"), "{}", b.display());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&dir).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o700, "private to the user");
        }
        std::fs::remove_dir_all(&base).unwrap();
    }

    #[test]
    fn sweep_takes_day_old_images_and_their_empty_directories() {
        let base = scratch("sweep");
        let dead = base.join(format!("{DIR_PREFIX}11"));
        let live = base.join(format!("{DIR_PREFIX}22"));
        let other = base.join("someone-else");
        for d in [&dead, &live, &other] {
            std::fs::create_dir_all(d).unwrap();
        }
        std::fs::write(dead.join("paste-a.png"), b"x").unwrap();
        std::fs::write(live.join("paste-b.png"), b"x").unwrap();
        std::fs::write(other.join("keep.png"), b"x").unwrap();
        // Every file is "a day and a bit old" when now is two days on, but
        // only the paste directories are ours to sweep.
        let later = SystemTime::now() + Duration::from_secs(2 * 24 * 60 * 60);
        sweep(&base, later, STALE_AFTER);
        assert!(!dead.exists() && !live.exists(), "old pastes go");
        assert!(other.join("keep.png").exists(), "not ours: untouched");

        // A fresh paste stays, and so does its directory.
        std::fs::create_dir_all(&live).unwrap();
        std::fs::write(live.join("paste-c.png"), b"x").unwrap();
        sweep(&base, SystemTime::now(), STALE_AFTER);
        assert!(live.join("paste-c.png").exists());
        std::fs::remove_dir_all(&base).unwrap();
    }

    #[test]
    fn clean_up_removes_the_run_directory() {
        let base = scratch("cleanup");
        let (tx, rx) = crossbeam_channel::unbounded();
        let paste = ImagePaste {
            dir: base.join(format!("{DIR_PREFIX}33")),
            tx,
            rx,
        };
        save(&paste.dir, b"png", SystemTime::now()).unwrap();
        assert!(paste.dir.exists());
        let dir = paste.dir.clone();
        drop(paste);
        assert!(!dir.exists(), "the drop at exit takes the run's pastes");
        std::fs::remove_dir_all(&base).unwrap();
    }

    #[test]
    fn rgba_encodes_to_a_png() {
        let png = encode_png(2, 1, &[255, 0, 0, 255, 0, 255, 0, 128]).unwrap();
        assert!(png.starts_with(PNG_MAGIC));
    }
}
