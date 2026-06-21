// Prevents an extra console window on Windows in release.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    // On Wayland, WebKitGTK's DMABUF renderer does not present buffer updates for
    // a transparent, never-focused window until an unrelated ~20s refresh — so the
    // settings panel's DOM state flips (interaction follows it) but the old frame
    // stays on screen, looking like it won't close. Disabling the DMABUF renderer
    // (falls back to a path that presents on content change) fixes it; the
    // compositing-mode flag is kept as a companion mitigation.
    #[cfg(target_os = "linux")]
    {
        std::env::set_var("WEBKIT_DISABLE_DMABUF_RENDERER", "1");
        std::env::set_var("WEBKIT_DISABLE_COMPOSITING_MODE", "1");
    }

    spoke_lib::run();
}
