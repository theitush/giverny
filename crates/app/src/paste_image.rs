//! Pasting a screenshot (giverny#395).
//!
//! Ctrl+V (Cmd+V) with an image and no text in the clipboard used to do
//! nothing: egui only ever pastes text, and Claude Code's own clipboard read
//! needs `xclip`/`wl-paste`, which a WSL distro rarely has. So Giverny reads
//! the image itself, writes it to a PNG of its own, and pastes the file's
//! path the way a file drop types one: Claude Code attaches an image path
//! pasted into its prompt (`[Image #1]`), and a shell gets the path.
//!
//! Where the image comes from:
//! * the clipboard through `arboard` — X11 (or XWayland) on Linux,
//!   NSPasteboard on macOS;
//! * on WSL, when that has none, the Windows clipboard through interop.
//!   WSLg's clipboard bridge carries text only: a Win+Shift+S snip shows up
//!   in X as an empty `UTF8_STRING` and no image target at all.
//!
//! The read runs on a thread of its own, and a frame never waits on it.
//! On WSL it goes to a PowerShell kept running for the purpose
//! ([`WindowsClipboard`], giverny#410): a fresh `powershell.exe` took 1.4 to
//! 2.7 s to start, which was nearly all of a paste's time; the resident one
//! answers in about 10 ms. It is started a moment after the app, and it
//! exits with it. An answer that does not come in time (a clipboard held
//! open, a copier that never renders what it offered) costs that paste
//! a few seconds and that reader its life (giverny#420): the next paste
//! starts a fresh one rather than queueing behind a hung read.
//!
//! The PNGs live in a directory of this run's (`giverny-paste-<pid>` under
//! `$XDG_RUNTIME_DIR`, or the temp dir), removed when the app exits; files
//! a crashed run left behind go at the next start once they are a day old.

use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};

use crossbeam_channel::{Receiver, RecvTimeoutError, Sender};
use giverny_core::tabs::TabId;

/// The prefix of every run's paste directory, which the start-up sweep
/// looks for.
const DIR_PREFIX: &str = "giverny-paste-";

/// Pasted images older than this are a dead run's, and swept at start.
const STALE_AFTER: Duration = Duration::from_secs(24 * 60 * 60);

/// The eight bytes every PNG starts with.
const PNG_MAGIC: &[u8] = b"\x89PNG\r\n\x1a\n";

/// The resident PowerShell's script: for each line read on stdin, one line
/// on stdout with the Windows clipboard's image as base64 PNG — the `PNG`
/// format itself when the copier offered one (a Win+Shift+S snip does, alpha
/// and all), so it passes through untouched, else the bitmap encoded — and
/// an empty line when there is no image (or the clipboard would not open).
/// stdin's end is its end. The format query before the loop only warms it
/// up: the first paste's read took 80 ms without it, 25 ms with.
const WINDOWS_CLIPBOARD_PS: &str = "\
Add-Type -AssemblyName System.Windows.Forms,System.Drawing; \
$null = [Windows.Forms.Clipboard]::ContainsImage(); \
while ($null -ne [Console]::In.ReadLine()) { \
$m = New-Object IO.MemoryStream; \
try { $d = [Windows.Forms.Clipboard]::GetDataObject(); \
if ($d -and $d.GetDataPresent('PNG')) { $d.GetData('PNG').CopyTo($m) } \
elseif ([Windows.Forms.Clipboard]::ContainsImage()) { \
[Windows.Forms.Clipboard]::GetImage().Save($m, [Drawing.Imaging.ImageFormat]::Png) } } \
catch { $m.SetLength(0) }; \
[Console]::Out.WriteLine([Convert]::ToBase64String($m.ToArray())); \
[Console]::Out.Flush() }";

/// How long after the app starts the resident PowerShell is started: out of
/// the way of the first frames, and long before anyone pastes.
const WARM_UP_AFTER: Duration = Duration::from_secs(2);

/// How long a running reader has to answer a paste: a read takes 10 to
/// 25 ms, so one this late is stuck, and is ended rather than waited on.
const ANSWER_WITHIN: Duration = Duration::from_secs(3);

/// How long a reader just started has for its first answer, which waits on
/// PowerShell's own start: 1.4 to 2.7 s, 4.5 s seen with Windows busy.
const FIRST_ANSWER_WITHIN: Duration = Duration::from_secs(15);

/// A pasted image's file, ready to be typed into the tab it was pasted in.
pub struct Pasted {
    pub tab: TabId,
    pub path: PathBuf,
}

/// The app's side of image pastes: the run's directory, and the threads'
/// answers.
pub struct ImagePaste {
    /// This run's directory; created on the first paste.
    dir: PathBuf,
    tx: Sender<Pasted>,
    rx: Receiver<Pasted>,
    /// WSL only: the Windows clipboard's reader, kept running.
    windows: Option<Arc<WindowsClipboard>>,
    /// Debug builds: the next paste reads this PNG instead of the clipboard
    /// (`GIVERNY_DEBUG_CMD` `paste-image <file>`), so the whole path can be
    /// driven in a test window without touching anyone's clipboard.
    #[cfg(debug_assertions)]
    pub debug_source: Option<PathBuf>,
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
        let windows = giverny_claude::away::is_wsl().then(|| {
            let windows = Arc::new(WindowsClipboard::new(powershell_command));
            windows.warm_up_after(WARM_UP_AFTER);
            windows
        });
        Self {
            dir: base.join(format!("{DIR_PREFIX}{}", std::process::id())),
            tx,
            rx,
            windows,
            #[cfg(debug_assertions)]
            debug_source: None,
        }
    }

    /// Look for an image in the clipboard, in the background, and when there
    /// is one have it pasted into `tab` (see [`Self::poll`]).
    pub fn request(&mut self, ctx: &egui::Context, tab: TabId) {
        #[cfg(debug_assertions)]
        let debug_source = self.debug_source.take();
        let dir = self.dir.clone();
        let tx = self.tx.clone();
        let windows = self.windows.clone();
        let ctx = ctx.clone();
        let asked = std::time::Instant::now();
        let spawned = std::thread::Builder::new()
            .name("paste image".into())
            .spawn(move || {
                #[cfg(debug_assertions)]
                let png = match debug_source {
                    Some(file) => std::fs::read(&file)
                        .inspect_err(|e| tracing::warn!("paste-image {}: {e}", file.display()))
                        .ok(),
                    None => clipboard_png(windows.as_deref()),
                };
                #[cfg(not(debug_assertions))]
                let png = clipboard_png(windows.as_deref());
                let Some(png) = png.filter(|b| b.starts_with(PNG_MAGIC)) else {
                    tracing::info!("paste: no text and no image in the clipboard");
                    return;
                };
                match save(&dir, &png, SystemTime::now()) {
                    Ok(path) => {
                        tracing::info!(
                            "paste: image saved to {} in {} ms",
                            path.display(),
                            asked.elapsed().as_millis()
                        );
                        let _ = tx.send(Pasted { tab, path });
                        ctx.request_repaint();
                    }
                    Err(e) => tracing::warn!("paste: image not saved in {}: {e}", dir.display()),
                }
            });
        if let Err(e) = spawned {
            tracing::warn!("paste: no thread for the image read: {e}");
        }
    }

    /// Images read and saved since the last call, oldest first.
    pub fn poll(&self) -> Vec<Pasted> {
        self.rx.try_iter().collect()
    }

    /// Remove this run's pasted images, and stop the Windows clipboard's
    /// reader. Run at exit: by then Claude Code has long read every image
    /// it was given.
    pub fn clean_up(&self) {
        if let Some(windows) = &self.windows {
            windows.shut_down();
        }
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

/// The clipboard's image as PNG bytes, from wherever this platform keeps it:
/// through arboard first (a few milliseconds when it has none), then on WSL
/// the Windows clipboard.
fn clipboard_png(windows: Option<&WindowsClipboard>) -> Option<Vec<u8>> {
    if let Some(png) = arboard_png() {
        return Some(png);
    }
    windows?.png()
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

/// WSL: the Windows clipboard, read by a PowerShell over interop that is
/// kept running between pastes, so that a paste costs a round trip (about
/// 10 ms) rather than PowerShell's start (1.4 to 2.7 s). Base64 on a pipe
/// rather than a file Windows writes: no path to translate, and nothing of
/// Windows' to clean up.
///
/// Pastes take turns at it. One that finds it gone (its stdin's end is its
/// end, so a crash of ours ends it too) starts another and asks once more.
/// One that gets no answer in time ends it and has no image; the next
/// paste starts another.
struct WindowsClipboard {
    /// How to start the reader: [`powershell_command`], or a stand-in in
    /// tests.
    command: fn() -> Command,
    /// [`ANSWER_WITHIN`] and [`FIRST_ANSWER_WITHIN`], or shorter in tests.
    answer_within: Duration,
    first_answer_within: Duration,
    reader: Mutex<Option<Reader>>,
}

/// A running reader, its stdin, and its stdout's lines as a thread of its
/// own reads them, so that waiting on one can have a deadline.
struct Reader {
    child: Child,
    stdin: ChildStdin,
    lines: Receiver<std::io::Result<String>>,
    /// Whether it has answered yet: its first answer waits on its start.
    answered: bool,
}

/// Why a reader gave no answer.
#[derive(Debug)]
enum AskError {
    /// It is gone: its pipe broke or closed.
    Gone(std::io::Error),
    /// It is there but did not answer in time.
    Late,
}

impl WindowsClipboard {
    fn new(command: fn() -> Command) -> Self {
        Self {
            command,
            answer_within: ANSWER_WITHIN,
            first_answer_within: FIRST_ANSWER_WITHIN,
            reader: Mutex::new(None),
        }
    }

    /// Start the reader after `delay`, on a thread of its own, unless a
    /// paste has started it first.
    fn warm_up_after(self: &Arc<Self>, delay: Duration) {
        let this = Arc::clone(self);
        let spawned = std::thread::Builder::new()
            .name("paste warm-up".into())
            .spawn(move || {
                std::thread::sleep(delay);
                let mut reader = this.lock();
                if reader.is_none() {
                    *reader = this.start();
                }
            });
        if let Err(e) = spawned {
            tracing::debug!("paste: no thread to start the clipboard reader: {e}");
        }
    }

    /// The clipboard's image as PNG bytes, if it holds one. Waits for the
    /// reader's start when it has not finished starting.
    fn png(&self) -> Option<Vec<u8>> {
        let mut reader = self.lock();
        for attempt in 0..2 {
            if reader.is_none() {
                *reader = self.start();
            }
            let current = reader.as_mut()?;
            let within = if current.answered {
                self.answer_within
            } else {
                self.first_answer_within
            };
            match current.ask(within) {
                Ok(line) => return decode_base64_png(&line),
                Err(AskError::Gone(e)) => {
                    tracing::info!("paste: the clipboard reader is gone ({e}); attempt {attempt}");
                    if let Some(mut gone) = reader.take() {
                        gone.stop();
                    }
                }
                Err(AskError::Late) => {
                    // Asking again would wait as long once more; the next
                    // paste starts a fresh reader instead.
                    tracing::warn!(
                        "paste: the clipboard reader did not answer in {} ms; ended",
                        within.as_millis()
                    );
                    if let Some(mut stuck) = reader.take() {
                        stuck.stop();
                    }
                    return None;
                }
            }
        }
        None
    }

    /// Stop the reader, if it is running. A paste after this starts another.
    fn shut_down(&self) {
        if let Some(mut reader) = self.lock().take() {
            reader.stop();
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Option<Reader>> {
        // A paste thread that panicked left nothing half-done that matters:
        // at worst a reader whose next answer is out of step, which the
        // next ask replaces when it fails.
        self.reader.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn start(&self) -> Option<Reader> {
        let mut child = (self.command)()
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .inspect_err(|e| tracing::warn!("paste: the clipboard reader did not start: {e}"))
            .ok()?;
        let (Some(stdin), Some(stdout)) = (child.stdin.take(), child.stdout.take()) else {
            let _ = child.kill();
            let _ = child.wait();
            return None;
        };
        // The thread ends with the reader: a killed reader's stdout closes.
        let (tx, lines) = crossbeam_channel::unbounded();
        let spawned = std::thread::Builder::new()
            .name("paste reader".into())
            .spawn(move || {
                let mut stdout = BufReader::new(stdout);
                loop {
                    let mut line = String::new();
                    let read = match stdout.read_line(&mut line) {
                        Ok(0) => return,
                        Ok(_) => Ok(line.trim_end().to_string()),
                        Err(e) => Err(e),
                    };
                    let failed = read.is_err();
                    if tx.send(read).is_err() || failed {
                        return;
                    }
                }
            });
        if let Err(e) = spawned {
            tracing::warn!("paste: no thread to read the clipboard reader: {e}");
            let _ = child.kill();
            let _ = child.wait();
            return None;
        }
        Some(Reader {
            child,
            stdin,
            lines,
            answered: false,
        })
    }
}

impl Drop for WindowsClipboard {
    fn drop(&mut self) {
        self.shut_down();
    }
}

impl Reader {
    /// One request, one line back (without its line end), if it comes
    /// `within` that long.
    fn ask(&mut self, within: Duration) -> Result<String, AskError> {
        self.stdin
            .write_all(b"\n")
            .and_then(|()| self.stdin.flush())
            .map_err(AskError::Gone)?;
        match self.lines.recv_timeout(within) {
            Ok(Ok(line)) => {
                self.answered = true;
                Ok(line)
            }
            Ok(Err(e)) => Err(AskError::Gone(e)),
            Err(RecvTimeoutError::Disconnected) => {
                Err(AskError::Gone(std::io::ErrorKind::UnexpectedEof.into()))
            }
            Err(RecvTimeoutError::Timeout) => Err(AskError::Late),
        }
    }

    fn stop(&mut self) {
        // Its stdin closes with the drop; the kill makes sure, and ending
        // the Linux side of an interop process ends the Windows side.
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// The resident PowerShell: Windows' own, by its full path when the C:
/// drive is mounted where WSL mounts it, else whatever `PATH` finds. Started
/// in C:\ so it does not hold a UNC path into this distro.
fn powershell_command() -> Command {
    let ps = ["/mnt/c/Windows/System32/WindowsPowerShell/v1.0/powershell.exe"]
        .into_iter()
        .find(|p| Path::new(p).exists())
        .unwrap_or("powershell.exe");
    let mut command = Command::new(ps);
    // The clipboard is an OLE service: single-threaded apartment only.
    command
        .args(["-NoProfile", "-NonInteractive", "-Sta", "-Command"])
        .arg(WINDOWS_CLIPBOARD_PS);
    if Path::new("/mnt/c").is_dir() {
        command.current_dir("/mnt/c");
    }
    command
}

/// A reader's answer as PNG bytes: `None` for the empty line of "no image".
fn decode_base64_png(line: &str) -> Option<Vec<u8>> {
    use base64::Engine;
    if line.is_empty() {
        return None;
    }
    base64::engine::general_purpose::STANDARD
        .decode(line)
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
            windows: None,
            #[cfg(debug_assertions)]
            debug_source: None,
        };
        save(&paste.dir, b"png", SystemTime::now()).unwrap();
        assert!(paste.dir.exists());
        let dir = paste.dir.clone();
        drop(paste);
        assert!(!dir.exists(), "the drop at exit takes the run's pastes");
        std::fs::remove_dir_all(&base).unwrap();
    }

    /// A one-pixel PNG as base64, as a reader answers.
    #[cfg(unix)]
    fn one_pixel_base64() -> String {
        use base64::Engine;
        let png = encode_png(1, 1, &[1, 2, 3, 255]).unwrap();
        base64::engine::general_purpose::STANDARD.encode(png)
    }

    /// A stand-in reader: answers every line with a one-pixel PNG.
    #[cfg(unix)]
    fn fake_reader() -> Command {
        let mut command = Command::new("sh");
        let b64 = one_pixel_base64();
        command.args(["-c", &format!("while read l; do echo {b64}; done")]);
        command
    }

    /// A stand-in reader that has no image to give.
    #[cfg(unix)]
    fn empty_reader() -> Command {
        let mut command = Command::new("sh");
        command.args(["-c", "while read l; do echo; done"]);
        command
    }

    #[cfg(unix)]
    #[test]
    fn the_reader_stays_up_between_pastes_and_comes_back_when_gone() {
        let windows = WindowsClipboard::new(fake_reader);
        let first = windows.png().expect("an image");
        assert!(first.starts_with(PNG_MAGIC));
        let pid = windows.lock().as_ref().unwrap().child.id();
        assert_eq!(windows.png(), Some(first.clone()));
        assert_eq!(
            windows.lock().as_ref().unwrap().child.id(),
            pid,
            "the same reader answers the next paste"
        );
        // Gone under us: the next paste starts another and still gets its
        // image.
        windows.lock().as_mut().unwrap().child.kill().unwrap();
        assert_eq!(windows.png(), Some(first));
        assert_ne!(windows.lock().as_ref().unwrap().child.id(), pid);
        windows.shut_down();
        assert!(windows.lock().is_none());
    }

    /// A stand-in reader that answers its first paste and hangs on the
    /// second, the way a clipboard held open would hang a read.
    #[cfg(unix)]
    fn hangs_on_the_second_paste() -> Command {
        let mut command = Command::new("sh");
        let b64 = one_pixel_base64();
        // exec: the kill lands on the sleep itself, leaving no orphan.
        command.args(["-c", &format!("read l; echo {b64}; read l; exec sleep 999")]);
        command
    }

    #[cfg(unix)]
    #[test]
    fn a_hung_read_costs_one_paste_its_deadline_and_the_next_paste_works() {
        let windows = WindowsClipboard::new(hangs_on_the_second_paste);
        let first = windows.png().expect("the first paste is answered");
        let pid = windows.lock().as_ref().unwrap().child.id();

        let asked = std::time::Instant::now();
        assert_eq!(windows.png(), None, "a hung read is no image");
        let took = asked.elapsed();
        assert!(
            took >= ANSWER_WITHIN && took < ANSWER_WITHIN + Duration::from_secs(2),
            "one paste costs about the deadline: {took:?}"
        );
        assert!(windows.lock().is_none(), "the stuck reader is ended");
        let gone = Path::new("/proc").join(pid.to_string());
        assert!(!gone.exists(), "and its process with it");

        let asked = std::time::Instant::now();
        assert_eq!(windows.png(), Some(first), "the next paste works");
        assert!(asked.elapsed() < ANSWER_WITHIN, "{:?}", asked.elapsed());
        assert_ne!(windows.lock().as_ref().unwrap().child.id(), pid);
        windows.shut_down();
    }

    #[cfg(unix)]
    #[test]
    fn a_reader_that_never_answers_is_ended_at_its_first_deadline() {
        fn never_answers() -> Command {
            let mut command = Command::new("sh");
            command.args(["-c", "read l; exec sleep 999"]);
            command
        }
        let mut windows = WindowsClipboard::new(never_answers);
        windows.first_answer_within = Duration::from_millis(300);
        let asked = std::time::Instant::now();
        assert_eq!(windows.png(), None);
        assert!(
            asked.elapsed() < Duration::from_secs(2),
            "{:?}",
            asked.elapsed()
        );
        assert!(windows.lock().is_none(), "ended, not asked twice");
    }

    #[cfg(unix)]
    #[test]
    fn an_empty_answer_is_no_image() {
        let windows = WindowsClipboard::new(empty_reader);
        assert_eq!(windows.png(), None);
        assert!(windows.lock().is_some(), "kept for the next paste");
    }

    #[test]
    fn a_reader_that_will_not_start_is_no_image() {
        fn missing() -> Command {
            Command::new("/nonexistent/giverny-test-reader")
        }
        assert_eq!(WindowsClipboard::new(missing).png(), None);
    }

    #[test]
    fn rgba_encodes_to_a_png() {
        let png = encode_png(2, 1, &[255, 0, 0, 255, 0, 255, 0, 128]).unwrap();
        assert!(png.starts_with(PNG_MAGIC));
    }
}
