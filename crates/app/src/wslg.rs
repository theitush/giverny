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

use std::io::Read;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// The hidden subcommand that runs [`probe`].
pub const PROBE_ARG: &str = "gl-probe";

/// Longer than the probe takes (~0.1 s), short enough not to stall a launch.
const PROBE_TIMEOUT: Duration = Duration::from_secs(3);

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
pub fn enable_d3d12() -> Gpu {
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
        let why = err.lines().last().unwrap_or("no output").trim();
        format!("probe failed ({status}): {why}")
    })
}

fn parse_probe(stdout: &str) -> Option<String> {
    stdout
        .lines()
        .find_map(|l| l.strip_prefix("renderer: "))
        .map(|r| r.trim().to_string())
        .filter(|r| !r.is_empty())
}

/// `giverny gl-probe`: make an OpenGL ES context with no window and print the
/// renderer it landed on. Run by [`enable_d3d12`] with the driver to try set.
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
    use khronos_egl as egl;
    const PLATFORM_SURFACELESS_MESA: egl::Enum = 0x31DD;
    const GL_RENDERER: u32 = 0x1F01;

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
    let get_string = egl
        .get_proc_address("glGetString")
        .ok_or("no glGetString")?;
    // SAFETY: glGetString's signature, from a current context's GL.
    let get_string: extern "system" fn(u32) -> *const std::ffi::c_char =
        unsafe { std::mem::transmute(get_string) };
    let ptr = get_string(GL_RENDERER);
    if ptr.is_null() {
        return Err("glGetString(GL_RENDERER) is null".into());
    }
    // SAFETY: a NUL-terminated string owned by the GL implementation.
    let renderer = unsafe { std::ffi::CStr::from_ptr(ptr) }
        .to_string_lossy()
        .into_owned();
    let _ = egl.make_current(display, None, None, None);
    let _ = egl.destroy_context(display, context);
    let _ = egl.terminate(display);
    Ok(renderer)
}

#[cfg(test)]
mod tests {
    use super::*;

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
