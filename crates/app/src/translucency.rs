//! Whether the window can be see-through (`window.opacity`).
//!
//! Asking for a transparent window is only half of it: the surface has to
//! carry alpha, and something has to composite it. Where either is missing
//! the alpha is not ignored but shown — the background comes out darker by
//! however much was left out — so every way this can fail ends in painting
//! solid, not in painting anyway.

use eframe::egui;
use giverny_core::config::WindowConfig;

/// Whether to ask for a transparent window at all. Decided before it exists,
/// because the surface is created once: a window opened solid stays solid
/// until Giverny restarts.
pub fn request(cfg: &WindowConfig) -> bool {
    if !cfg.translucent() {
        return false;
    }
    #[cfg(all(unix, not(any(target_os = "macos", target_os = "android"))))]
    if crate::wslg::is_wslg() {
        // WSLg's compositor hands the window to Windows over RDP, and has
        // not been seen to carry its alpha through. Not tried, so not risked.
        tracing::info!("window.opacity: kept solid under WSLg");
        return false;
    }
    true
}

/// Whether the window that was opened can show what is behind it. Logs why
/// not, once, since this runs once.
pub fn confirm(cc: &eframe::CreationContext<'_>) -> bool {
    match check(cc) {
        Ok(()) => {
            tracing::info!("window.opacity: window is see-through");
            true
        }
        Err(why) => {
            tracing::warn!("window.opacity: drawing solid, {why}");
            false
        }
    }
}

fn check(cc: &eframe::CreationContext<'_>) -> Result<(), String> {
    #[cfg(all(unix, not(any(target_os = "macos", target_os = "android"))))]
    x11_compositor(cc)?;
    if let Some(gl) = &cc.gl {
        return gl_alpha(gl);
    }
    if let Some(wgpu) = &cc.wgpu_render_state {
        return wgpu_premultiplied(cc, &wgpu.adapter);
    }
    Err("no renderer to ask".into())
}

/// On X11 a transparent window is only transparent while a compositing
/// manager runs; without one the X server shows the premultiplied pixels as
/// they are. A compositor announces itself by owning `_NET_WM_CM_S<screen>`.
#[cfg(all(unix, not(any(target_os = "macos", target_os = "android"))))]
fn x11_compositor(cc: &eframe::CreationContext<'_>) -> Result<(), String> {
    use raw_window_handle::{HasDisplayHandle, RawDisplayHandle};
    use x11_dl::xlib::{Display, False, Xlib};

    let Ok(handle) = cc.display_handle() else {
        return Ok(());
    };
    let RawDisplayHandle::Xlib(handle) = handle.as_raw() else {
        // Wayland: the compositor is the display server.
        return Ok(());
    };
    let display = handle
        .display
        .ok_or("no Xlib display to ask for a compositor")?
        .as_ptr()
        .cast::<Display>();
    let xlib = Xlib::open().map_err(|e| e.to_string())?;
    let name = std::ffi::CString::new(format!("_NET_WM_CM_S{}", handle.screen))
        .map_err(|e| e.to_string())?;
    // SAFETY: `display` is winit's open connection, used here on the thread
    // winit uses it from, before the first frame.
    let owner = unsafe {
        let atom = (xlib.XInternAtom)(display, name.as_ptr(), False);
        (xlib.XGetSelectionOwner)(display, atom)
    };
    if owner == 0 {
        return Err("X11 has no compositing manager running".into());
    }
    Ok(())
}

/// The default framebuffer has to have an alpha channel for anything but
/// solid to reach the compositor. eframe asks for one, and logs when the
/// driver offered none.
fn gl_alpha(gl: &eframe::glow::Context) -> Result<(), String> {
    use eframe::glow::{self, HasContext};
    // SAFETY: eframe makes the window's context current before creating the
    // app, which is when this runs. Only queries, and the binding it sets is
    // the one eframe paints into.
    unsafe {
        while gl.get_error() != glow::NO_ERROR {}
        gl.bind_framebuffer(glow::FRAMEBUFFER, None);
        // Desktop GL names the default back buffer BACK_LEFT; GLES, BACK.
        for attachment in [glow::BACK_LEFT, glow::BACK] {
            let bits = gl.get_framebuffer_attachment_parameter_i32(
                glow::FRAMEBUFFER,
                attachment,
                glow::FRAMEBUFFER_ATTACHMENT_ALPHA_SIZE,
            );
            if gl.get_error() == glow::NO_ERROR {
                return if bits > 0 {
                    Ok(())
                } else {
                    Err("the OpenGL framebuffer has no alpha channel".into())
                };
            }
        }
    }
    Err("OpenGL would not say whether the framebuffer has alpha".into())
}

/// egui's output is premultiplied, so the surface has to be composited as
/// premultiplied. eframe settles for post-multiplied where that is all there
/// is, which multiplies every pixel by its alpha a second time: a darker
/// background and dark edges on text. So pre-multiplied or nothing.
///
/// eframe keeps its surface to itself, so this asks through a second one on
/// the same window, from a second instance of the same backend: a surface
/// is not a swapchain, and wgpu allows several per window.
fn wgpu_premultiplied(
    cc: &eframe::CreationContext<'_>,
    adapter: &eframe::wgpu::Adapter,
) -> Result<(), String> {
    use eframe::wgpu;
    let info = adapter.get_info();
    let setup = eframe::egui_wgpu::WgpuSetupCreateNew::without_display_handle();
    let mut descriptor = setup.instance_descriptor;
    descriptor.backends = wgpu::Backends::from(info.backend);
    let instance = wgpu::Instance::new(descriptor);
    // SAFETY: the window and display outlive the surface, which is dropped
    // at the end of this function.
    let surface = unsafe {
        let target = wgpu::SurfaceTargetUnsafe::from_display_and_window(cc, cc)
            .map_err(|e| e.to_string())?;
        instance.create_surface_unsafe(target)
    }
    .map_err(|e| e.to_string())?;
    let same = pollster::block_on(instance.enumerate_adapters(info.backend.into()))
        .into_iter()
        .find(|a| {
            let other = a.get_info();
            other.name == info.name && other.device == info.device && other.vendor == info.vendor
        })
        .ok_or_else(|| format!("{} not found a second time", info.name))?;
    let modes = surface.get_capabilities(&same).alpha_modes;
    if modes.contains(&wgpu::CompositeAlphaMode::PreMultiplied) {
        Ok(())
    } else {
        Err(format!(
            "the wgpu surface cannot be composited premultiplied (offers {modes:?})"
        ))
    }
}

/// What eframe clears the frame to. A see-through window starts every frame
/// from nothing, so the panels' alpha is the only alpha; a solid one keeps
/// eframe's own default, the value it has always been cleared to.
pub fn clear_color(see_through: bool) -> [f32; 4] {
    if see_through {
        [0.0; 4]
    } else {
        egui::Color32::from_rgba_unmultiplied(12, 12, 12, 180).to_normalized_gamma_f32()
    }
}
