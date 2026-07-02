//! Runtime OS-permission checks, surfaced to the UI as warnings.
//!
//! macOS is the only supported OS with a queryable permission model for what
//! Spoke needs (microphone via TCC, keystroke injection via Accessibility).
//! Other platforms report `Unknown`, which the UI treats as "no warning" —
//! add a platform arm here if one grows a queryable API.

use serde::Serialize;

// Each target OS only constructs a subset of variants; allow the rest.
#[allow(dead_code)]
#[derive(Serialize, Clone, Copy, PartialEq, Eq, Debug)]
#[serde(rename_all = "snake_case")]
pub enum PermissionState {
    Granted,
    Denied,
    /// Not asked yet — the OS will prompt on first use, so no warning needed.
    Undetermined,
    /// This platform has no queryable permission model.
    Unknown,
}

#[derive(Serialize)]
pub struct Permissions {
    pub microphone: PermissionState,
    pub accessibility: PermissionState,
}

pub fn check() -> Permissions {
    Permissions {
        microphone: microphone(),
        accessibility: accessibility(),
    }
}

/// Open the OS settings pane where the user can grant the given permission
/// ("microphone" | "accessibility"). No-op on platforms without one.
pub fn open_settings(which: &str) {
    #[cfg(target_os = "macos")]
    {
        let url = match which {
            "microphone" => {
                "x-apple.systempreferences:com.apple.preference.security?Privacy_Microphone"
            }
            "accessibility" => {
                "x-apple.systempreferences:com.apple.preference.security?Privacy_Accessibility"
            }
            _ => return,
        };
        let _ = std::process::Command::new("open").arg(url).spawn();
    }
    #[cfg(not(target_os = "macos"))]
    let _ = which;
}

#[cfg(target_os = "macos")]
fn microphone() -> PermissionState {
    use objc2::msg_send;
    use objc2_foundation::NSString;

    // Force-link AVFoundation so the AVCaptureDevice class is registered.
    #[link(name = "AVFoundation", kind = "framework")]
    extern "C" {}

    // [AVCaptureDevice authorizationStatusForMediaType:AVMediaTypeAudio]
    // AVMediaTypeAudio is the constant string "soun".
    let media = NSString::from_str("soun");
    let status: isize = unsafe {
        msg_send![objc2::class!(AVCaptureDevice), authorizationStatusForMediaType: &*media]
    };
    match status {
        3 => PermissionState::Granted,      // authorized
        0 => PermissionState::Undetermined, // notDetermined
        _ => PermissionState::Denied,       // denied (2) / restricted (1)
    }
}

#[cfg(target_os = "macos")]
fn accessibility() -> PermissionState {
    #[link(name = "ApplicationServices", kind = "framework")]
    extern "C" {
        fn AXIsProcessTrusted() -> bool;
    }
    if unsafe { AXIsProcessTrusted() } {
        PermissionState::Granted
    } else {
        PermissionState::Denied
    }
}

#[cfg(not(target_os = "macos"))]
fn microphone() -> PermissionState {
    PermissionState::Unknown
}

#[cfg(not(target_os = "macos"))]
fn accessibility() -> PermissionState {
    PermissionState::Unknown
}
