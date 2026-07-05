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
    /// Not asked yet — the OS will prompt on first use.
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

/// Show the native macOS microphone permission prompt if the user hasn't been
/// asked yet, and report the final status through `on_done`.
///
/// `AVCaptureDevice requestAccessForMediaType:` is the only way to trigger the
/// standard mic prompt deliberately. If the status is already determined
/// (granted or denied) the completion handler fires immediately with the
/// current value, so this is safe to call unconditionally. A grant made through
/// this prompt takes effect in the running process — no restart needed.
#[cfg(target_os = "macos")]
pub fn request_microphone(on_done: impl Fn(bool) + Send + 'static) {
    use objc2::msg_send;
    use objc2_foundation::NSString;

    #[link(name = "AVFoundation", kind = "framework")]
    extern "C" {}

    let block = block2::RcBlock::new(move |granted: objc2::runtime::Bool| {
        on_done(granted.as_bool());
    });
    let media = NSString::from_str("soun");
    let _: () = unsafe {
        msg_send![
            objc2::class!(AVCaptureDevice),
            requestAccessForMediaType: &*media,
            completionHandler: &*block
        ]
    };
}

/// Drop this app's TCC entry for the given permission so the OS treats it as
/// never-asked. This is the documented escape hatch for a stale grant: an
/// ad-hoc-signed binary changes its code hash on every rebuild, so System
/// Settings can show Spoke as enabled while TCC no longer matches the running
/// binary. Resetting and re-requesting registers the current binary cleanly.
pub fn reset(which: &str, bundle_id: &str) {
    #[cfg(target_os = "macos")]
    {
        let service = match which {
            "microphone" => "Microphone",
            "accessibility" => "Accessibility",
            _ => return,
        };
        let _ = std::process::Command::new("tccutil")
            .args(["reset", service, bundle_id])
            .status();
    }
    #[cfg(not(target_os = "macos"))]
    let _ = (which, bundle_id);
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

/// Prompt the user with the standard macOS accessibility permission dialog.
///
/// This calls `AXIsProcessTrustedWithOptions` with
/// `kAXTrustedCheckOptionPrompt=true`, which shows the "… would like to
/// control this computer" dialog and properly registers the **current**
/// process with the TCC database.  This is *not* the same as just opening
/// System Settings — the OS-level prompt ensures the grant is associated
/// with the exact running binary.
#[cfg(target_os = "macos")]
pub fn request_accessibility() {
    use std::ffi::CString;

    type CFAllocatorRef = *const std::ffi::c_void;
    type CFStringRef = *const std::ffi::c_void;
    type CFDictionaryRef = *const std::ffi::c_void;

    #[link(name = "ApplicationServices", kind = "framework")]
    extern "C" {
        fn AXIsProcessTrustedWithOptions(options: CFDictionaryRef) -> bool;
    }

    #[link(name = "CoreFoundation", kind = "framework")]
    extern "C" {
        static kCFBooleanTrue: *const std::ffi::c_void;
        static kCFTypeDictionaryKeyCallBacks: std::ffi::c_void;
        static kCFTypeDictionaryValueCallBacks: std::ffi::c_void;

        fn CFStringCreateWithCString(
            alloc: CFAllocatorRef,
            cStr: *const std::os::raw::c_char,
            encoding: u32,
        ) -> CFStringRef;
        fn CFDictionaryCreate(
            alloc: CFAllocatorRef,
            keys: *const *const std::ffi::c_void,
            values: *const *const std::ffi::c_void,
            numItems: isize,
            keyCallBacks: *const std::ffi::c_void,
            valueCallBacks: *const std::ffi::c_void,
        ) -> CFDictionaryRef;
        fn CFRelease(cf: *const std::ffi::c_void);
    }

    #[allow(non_upper_case_globals)]
    const kCFStringEncodingUTF8: u32 = 0x08000100;

    unsafe {
        let key_cstr = CString::new("AXTrustedCheckOptionPrompt").unwrap();
        let key = CFStringCreateWithCString(
            std::ptr::null(),
            key_cstr.as_ptr(),
            kCFStringEncodingUTF8,
        );
        if key.is_null() {
            return;
        }

        let key_ptr = key;
        let value_ptr = kCFBooleanTrue;

        let dict = CFDictionaryCreate(
            std::ptr::null(),
            &key_ptr,
            &value_ptr,
            1,
            &kCFTypeDictionaryKeyCallBacks as *const _,
            &kCFTypeDictionaryValueCallBacks as *const _,
        );

        if !dict.is_null() {
            AXIsProcessTrustedWithOptions(dict);
            CFRelease(dict);
        }
        CFRelease(key);
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
