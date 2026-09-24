//! WSLg: reach the GPU through Mesa's d3d12 driver.
//!
//! Under WSL2 the GPU is a paravirtual device (`/dev/dxg`) with no DRM render
//! node, so Mesa's loader finds nothing to open and falls back to the software
//! rasterizers: lavapipe for Vulkan, llvmpipe for OpenGL. Every frame is then
//! drawn on the CPU, which on a large window costs several cores (#24, #35).
//! Mesa ships a driver that goes through `/dev/dxg` to the Windows GPU driver,
//! `d3d12`, but it is only used when asked for by name: `GALLIUM_DRIVER=d3d12`.
//! There is no d3d12 Vulkan driver (dzn) in most distributions' Mesa, so this
//! helps OpenGL only, and a WSLg window that gets it opens on OpenGL.
//!
//! Naming a gallium driver that cannot start makes EGL fail outright rather
//! than fall back to llvmpipe, so the driver is tried first in a child process
//! (`giverny gl-probe`), with no window: a GPU driver too old for d3d12, or a
//! hung probe, then costs nothing but the probe.
//!
//! Starting is not enough. A window on WSLg is presented by copying each frame
//! from the GPU back to the CPU, and with an older Windows GPU driver (Intel
//! 32.0.101.5972 is one) d3d12 reads back zeros from any texture over 64 KiB:
//! the context comes up, draws correctly, and every window it opens is black
//! (#35, #48). So the probe also clears a 256x256 framebuffer to a known
//! colour and reads the whole of it back, and d3d12 is used only if every
//! pixel arrives.

use std::io::Read;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// The hidden subcommand that runs [`probe`].
pub const PROBE_ARG: &str = "gl-probe";

/// Well over what the probe takes (~1.4 s on d3d12, nearly all of it the
/// driver starting up), short enough that a hung probe does not stall a
/// launch for long.
const PROBE_TIMEOUT: Duration = Duration::from_secs(5);

/// Why the d3d12 driver was or was not switched on, for the log.
#[derive(Debug, PartialEq)]
pub enum Gpu {
    /// Not WSL, or the user already chose a Mesa driver.
    NotApplicable,
    /// Switched on; the GL renderer string the probe saw.
    D3d12(String),
    /// Tried and did not work; why.
    Unavailable(String),
}

/// On WSLg, switch Mesa to its d3d12 driver if a probe shows it works.
///
/// Must run before anything loads EGL or GL, and while the process is still
/// single-threaded: it sets `GALLIUM_DRIVER`, which Mesa reads when it creates
/// the screen. Child processes (the shells in each tab) inherit it, which is
/// what WSLg's own guidance sets for GL applications anyway.
///
/// On by default; `GIVERNY_GPU=software` (or `off`) keeps Mesa's choice.
pub fn enable_d3d12() -> Gpu {
    if matches!(
        std::env::var("GIVERNY_GPU").as_deref(),
        Ok("software" | "off" | "llvmpipe" | "0")
    ) {
        return Gpu::NotApplicable;
    }
    if !std::path::Path::new("/dev/dxg").exists() {
        return Gpu::NotApplicable;
    }
    // Someone has already chosen: respect it, including a choice of llvmpipe.
    for key in [
        "GALLIUM_DRIVER",
        "LIBGL_ALWAYS_SOFTWARE",
        "MESA_LOADER_DRIVER_OVERRIDE",
    ] {
        if std::env::var_os(key).is_some() {
            return Gpu::NotApplicable;
        }
    }
    match run_probe() {
        Ok(renderer) if is_d3d12(&renderer) => {
            // SAFETY: called from main before any thread is started.
            unsafe { std::env::set_var("GALLIUM_DRIVER", "d3d12") };
            Gpu::D3d12(renderer)
        }
        Ok(renderer) => Gpu::Unavailable(format!("the probe got {renderer}")),
        Err(why) => Gpu::Unavailable(why),
    }
}

/// Whether to open on XWayland rather than WSLg's Wayland (#46).
///
/// WSLg's compositor is a weston 9 fork that segfaults under a Wayland client
/// that grows a `wl_shm_pool` while buffers from it are on screen: it keeps
/// pointers into the pool's old mapping, and the resize moves it. winit's
/// client-side decorations (sctk-adwaita) and its cursor theme both keep
/// their buffers in pools that grow that way, so a Giverny window took the
/// whole WSLg session down with it seconds or minutes after it opened. Under
/// XWayland none of those pools exist. `GIVERNY_WAYLAND=1` opens on Wayland
/// anyway.
pub fn avoid_wayland() -> bool {
    avoid_wayland_for(
        std::env::var_os("WSL_DISTRO_NAME").is_some(),
        std::path::Path::new("/mnt/wslg").is_dir(),
        std::env::var_os("GIVERNY_WAYLAND").is_some(),
    )
}

fn avoid_wayland_for(wsl: bool, wslg: bool, wayland_asked_for: bool) -> bool {
    wsl && wslg && !wayland_asked_for
}

/// Windows' display scale, as WSLg's compositor last heard it (175% → 1.75).
///
/// WSLg hands X11 clients a display at scale 1 whose XRandR size is 0 mm, so
/// winit has no DPI to derive a scale from and reports 1.0: on a 175% laptop
/// panel every egui label comes out at little more than half its size (#62).
/// The real figure is in the RDP monitor layout Weston logs at startup and
/// on every display change; there is no X property or env var carrying it.
pub fn desktop_scale() -> Option<f32> {
    let log = std::fs::read_to_string("/mnt/wslg/weston.log").ok()?;
    parse_desktop_scale(&log)
}

/// The last non-zero `desktopScaleFactor:<percent>` in a Weston log. Zero is
/// what Weston logs before the client has told it anything.
fn parse_desktop_scale(log: &str) -> Option<f32> {
    log.lines()
        .rev()
        .filter_map(|line| {
            let rest = line.split("desktopScaleFactor:").nth(1)?;
            let digits: String = rest.chars().take_while(char::is_ascii_digit).collect();
            digits.parse::<u32>().ok()
        })
        .find(|&pct| pct != 0)
        .filter(|pct| (100..=500).contains(pct))
        .map(|pct| pct as f32 / 100.0)
}

/// The size of the framebuffer the probe reads back: 256 KiB, well over the
/// 64 KiB at which a broken driver starts returning zeros.
const PROBE_SIDE: i32 = 256;

/// The probe's clear colour. Every channel different and none of them 0 or
/// 255, so a swapped channel, a dropped alpha or a zeroed buffer all fail.
const PROBE_RGBA: [u8; 4] = [0x33, 0x66, 0x99, 0xcc];

/// What the probe prints when the context works but the readback does not;
/// the parent recognises it to tell the user what to do about it.
const READBACK_FAILED: &str = "readback failed";

/// Mesa's d3d12 renderer string is `D3D12 (<adapter name>)`.
fn is_d3d12(renderer: &str) -> bool {
    renderer.starts_with("D3D12")
}

fn run_probe() -> Result<String, String> {
    let exe = std::env::current_exe().map_err(|e| format!("no exe path: {e}"))?;
    let mut child = Command::new(exe)
        .arg(PROBE_ARG)
        .env("GALLIUM_DRIVER", "d3d12")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("probe did not start: {e}"))?;
    let deadline = Instant::now() + PROBE_TIMEOUT;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(10)),
            Ok(None) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!("probe still running after {PROBE_TIMEOUT:?}"));
            }
            Err(e) => return Err(format!("probe wait failed: {e}")),
        }
    };
    let mut out = String::new();
    let mut err = String::new();
    if let Some(mut s) = child.stdout.take() {
        let _ = s.read_to_string(&mut out);
    }
    if let Some(mut s) = child.stderr.take() {
        let _ = s.read_to_string(&mut err);
    }
    parse_probe(&out).ok_or_else(|| {
        let why = err
            .lines()
            .rev()
            .find(|l| l.starts_with("gl-probe: "))
            .or_else(|| err.lines().last())
            .unwrap_or("no output")
            .trim();
        let hint = if why.contains(READBACK_FAILED) {
            " (update the Windows GPU driver; an old one draws black windows)"
        } else {
            ""
        };
        format!("probe failed ({status}): {why}{hint}")
    })
}

fn parse_probe(stdout: &str) -> Option<String> {
    stdout
        .lines()
        .find_map(|l| l.strip_prefix("renderer: "))
        .map(|r| r.trim().to_string())
        .filter(|r| !r.is_empty())
}

/// `giverny gl-probe`: make an OpenGL ES context with no window, prove it can
/// read a framebuffer back (see the module docs), and print the renderer it
/// landed on. Run by [`enable_d3d12`] with the driver to try set.
pub fn probe() -> i32 {
    match probe_renderer() {
        Ok(renderer) => {
            println!("renderer: {renderer}");
            0
        }
        Err(why) => {
            eprintln!("gl-probe: {why}");
            1
        }
    }
}

fn probe_renderer() -> Result<String, String> {
    use eframe::glow::{self, HasContext};
    use khronos_egl as egl;
    const PLATFORM_SURFACELESS_MESA: egl::Enum = 0x31DD;

    // SAFETY: libEGL.so.1 is the system EGL; the symbols are checked on load.
    let egl = unsafe { egl::DynamicInstance::<egl::EGL1_5>::load_required() }
        .map_err(|e| format!("no libEGL: {e}"))?;
    // SAFETY: the surfaceless platform takes no native display.
    let display = unsafe {
        egl.get_platform_display(
            PLATFORM_SURFACELESS_MESA,
            std::ptr::null_mut(),
            &[egl::ATTRIB_NONE],
        )
    }
    .map_err(|e| format!("no surfaceless display: {e}"))?;
    egl.initialize(display)
        .map_err(|e| format!("eglInitialize: {e}"))?;
    egl.bind_api(egl::OPENGL_ES_API)
        .map_err(|e| format!("eglBindAPI: {e}"))?;
    // EGL_KHR_no_config_context: Mesa takes a null config for a context that
    // never draws to a surface.
    // SAFETY: a null config is what EGL_NO_CONFIG_KHR is.
    let no_config = unsafe { egl::Config::from_ptr(std::ptr::null_mut()) };
    let context = egl
        .create_context(
            display,
            no_config,
            None,
            &[egl::CONTEXT_CLIENT_VERSION, 2, egl::NONE],
        )
        .map_err(|e| format!("eglCreateContext: {e}"))?;
    egl.make_current(display, None, None, Some(context))
        .map_err(|e| format!("eglMakeCurrent: {e}"))?;
    // SAFETY: the context is current on this thread, and the loader hands
    // back that context's entry points (null for any it lacks).
    let gl = unsafe {
        glow::Context::from_loader_function(|name| {
            egl.get_proc_address(name)
                .map_or(std::ptr::null(), |f| f as *const std::ffi::c_void)
        })
    };
    // SAFETY: plain GL calls on the current context.
    let renderer = unsafe { gl.get_parameter_string(glow::RENDERER) };
    // SAFETY: as above.
    let readback = unsafe { read_back(&gl) };
    let _ = egl.make_current(display, None, None, None);
    let _ = egl.destroy_context(display, context);
    let _ = egl.terminate(display);
    if renderer.is_empty() {
        return Err("no GL_RENDERER".into());
    }
    readback.map_err(|why| format!("{READBACK_FAILED} on {renderer}: {why}"))?;
    Ok(renderer)
}

/// Clear a [`PROBE_SIDE`]-square framebuffer to [`PROBE_RGBA`] and read all of
/// it back.
///
/// # Safety
/// `gl` must be the current context's.
unsafe fn read_back(gl: &eframe::glow::Context) -> Result<(), String> {
    use eframe::glow::{self, HasContext};
    let side = PROBE_SIDE;
    // SAFETY: the caller's; every object made here is deleted before return.
    unsafe {
        let texture = gl.create_texture()?;
        gl.bind_texture(glow::TEXTURE_2D, Some(texture));
        gl.tex_image_2d(
            glow::TEXTURE_2D,
            0,
            glow::RGBA as i32,
            side,
            side,
            0,
            glow::RGBA,
            glow::UNSIGNED_BYTE,
            glow::PixelUnpackData::Slice(None),
        );
        let fbo = gl.create_framebuffer()?;
        gl.bind_framebuffer(glow::FRAMEBUFFER, Some(fbo));
        gl.framebuffer_texture_2d(
            glow::FRAMEBUFFER,
            glow::COLOR_ATTACHMENT0,
            glow::TEXTURE_2D,
            Some(texture),
            0,
        );
        let status = gl.check_framebuffer_status(glow::FRAMEBUFFER);
        let result = if status == glow::FRAMEBUFFER_COMPLETE {
            let [r, g, b, a] = PROBE_RGBA.map(|c| f32::from(c) / 255.0);
            gl.viewport(0, 0, side, side);
            gl.clear_color(r, g, b, a);
            gl.clear(glow::COLOR_BUFFER_BIT);
            gl.pixel_store_i32(glow::PACK_ALIGNMENT, 1);
            let mut pixels = vec![0u8; (side * side * 4) as usize];
            gl.read_pixels(
                0,
                0,
                side,
                side,
                glow::RGBA,
                glow::UNSIGNED_BYTE,
                glow::PixelPackData::Slice(Some(&mut pixels)),
            );
            match gl.get_error() {
                glow::NO_ERROR => check_pixels(&pixels, PROBE_RGBA),
                e => Err(format!("glReadPixels: GL error 0x{e:x}")),
            }
        } else {
            Err(format!("framebuffer incomplete (0x{status:x})"))
        };
        gl.bind_framebuffer(glow::FRAMEBUFFER, None);
        gl.delete_framebuffer(fbo);
        gl.delete_texture(texture);
        result
    }
}

/// Whether every RGBA pixel in `pixels` is `want`; if not, how many are not,
/// and the first one that differs.
fn check_pixels(pixels: &[u8], want: [u8; 4]) -> Result<(), String> {
    let (whole, rest) = pixels.as_chunks::<4>();
    if whole.is_empty() || !rest.is_empty() {
        return Err(format!(
            "{} bytes is not a whole number of pixels",
            pixels.len()
        ));
    }
    let total = whole.len();
    let mut wrong = whole.iter().filter(|p| **p != want);
    let Some(first) = wrong.next() else {
        return Ok(());
    };
    let wrong = 1 + wrong.count();
    Err(format!(
        "{wrong} of {total} pixels wrong (want {want:?}, got {first:?})"
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_readback_passes_only_when_every_pixel_is_the_clear_colour() {
        let want = PROBE_RGBA;
        let good: Vec<u8> = want.repeat(256 * 256);
        assert_eq!(check_pixels(&good, want), Ok(()));
        // What an old Intel driver returns over 64 KiB (#48): all zeros.
        let zeros = vec![0u8; 256 * 256 * 4];
        let why = check_pixels(&zeros, want).unwrap_err();
        assert!(why.starts_with("65536 of 65536 pixels wrong"), "{why}");
        // One bad pixel anywhere is enough, the last one included.
        let mut last = good.clone();
        let n = last.len();
        last[n - 1] = 0xff;
        assert!(
            check_pixels(&last, want)
                .unwrap_err()
                .starts_with("1 of 65536")
        );
        // A swapped channel order is a failure, not a pass.
        let bgra: Vec<u8> = [want[2], want[1], want[0], want[3]].repeat(4);
        assert!(check_pixels(&bgra, want).is_err());
        assert!(check_pixels(&[], want).is_err());
        assert!(check_pixels(&[1, 2, 3], want).is_err());
    }

    #[test]
    fn only_wslg_leaves_wayland_and_only_when_not_told_otherwise() {
        assert!(avoid_wayland_for(true, true, false));
        assert!(!avoid_wayland_for(true, true, true));
        // WSL without WSLg (an X server of its own, say), and not WSL at all.
        assert!(!avoid_wayland_for(true, false, false));
        assert!(!avoid_wayland_for(false, true, false));
        assert!(!avoid_wayland_for(false, false, false));
    }

    #[test]
    fn the_desktop_scale_is_the_last_one_weston_heard() {
        // Lines as WSLg's weston.log has them: a zero before the RDP client
        // reports, then the real figure, then a zero again on a relayout.
        let log = "[14:27:59.045] \trdpMonitor[0]: desktopScaleFactor:0, deviceScaleFactor:0\n\
                   [14:27:59.863] \trdpMonitor[0]: desktopScaleFactor:175, deviceScaleFactor:180\n\
                   [14:27:59.869] \trdpMonitor[0]: desktopScaleFactor:0, deviceScaleFactor:180\n";
        assert_eq!(parse_desktop_scale(log), Some(1.75));
        let later = format!("{log}[15:00:00.000] \trdpMonitor[0]: desktopScaleFactor:125, x\n");
        assert_eq!(parse_desktop_scale(&later), Some(1.25));
        assert_eq!(parse_desktop_scale(""), None);
        assert_eq!(
            parse_desktop_scale("rdpMonitor[0]: desktopScaleFactor:0, deviceScaleFactor:0\n"),
            None
        );
        assert_eq!(parse_desktop_scale("desktopScaleFactor:9000\n"), None);
    }

    #[test]
    fn only_a_d3d12_renderer_switches_the_driver() {
        assert!(is_d3d12("D3D12 (Intel(R) Graphics)"));
        assert!(is_d3d12("D3D12 (NVIDIA GeForce RTX 3060)"));
        assert!(!is_d3d12("llvmpipe (LLVM 20.1.2, 256 bits)"));
        assert!(!is_d3d12(""));
    }

    #[test]
    fn the_probe_output_is_read_from_its_renderer_line() {
        assert_eq!(
            parse_probe("renderer: D3D12 (Intel(R) Graphics)\n").as_deref(),
            Some("D3D12 (Intel(R) Graphics)")
        );
        assert_eq!(
            parse_probe("noise\nrenderer: llvmpipe\n").as_deref(),
            Some("llvmpipe")
        );
        assert_eq!(parse_probe(""), None);
        assert_eq!(parse_probe("renderer: \n"), None);
    }
}
