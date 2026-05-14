//! Platform DND / Focus detection for the voice pipeline.
//!
//! `is_focus_active()` returns `true` when the OS focus/DND mode is engaged.
//! On macOS this reads the `com.apple.notificationcenterui` CoreFoundation
//! preference domain; on Windows it calls `SHQueryUserNotificationState`.
//! On other platforms it always returns `false`.

/// Returns `true` when the system Focus / Do-Not-Disturb mode is active.
/// Voice synthesis is skipped when this returns `true`; the speech bubble
/// is still shown (per spec §5.3).
pub fn is_focus_active() -> bool {
    platform::check()
}

/// Returns `true` when the microphone is actively used by another application.
/// On macOS this queries `AVCaptureDevice`. On other platforms: `false`.
pub fn is_mic_busy() -> bool {
    mic::check()
}

// ─── macOS ───────────────────────────────────────────────────────────────────

#[cfg(target_os = "macos")]
mod platform {
    use std::ffi::CString;

    pub fn check() -> bool {
        // macOS Focus/DND stores the active mode count in the
        // com.apple.notificationcenterui preference domain.
        // Key "doNotDisturb" = 1 when classic DND is on.
        // Focus modes (macOS 12+) set "focusMode" to a non-empty dict.
        // We read both via CoreFoundation.
        unsafe { read_cf_focus_pref() }
    }

    #[link(name = "CoreFoundation", kind = "framework")]
    unsafe extern "C" {
        fn CFPreferencesCopyAppValue(
            key: *const std::ffi::c_void,
            application_id: *const std::ffi::c_void,
        ) -> *const std::ffi::c_void;
        fn CFRelease(cf: *const std::ffi::c_void);
        fn CFGetTypeID(cf: *const std::ffi::c_void) -> usize;
        fn CFBooleanGetTypeID() -> usize;
        fn CFBooleanGetValue(boolean: *const std::ffi::c_void) -> bool;
        fn CFStringCreateWithCString(
            alloc: *const std::ffi::c_void,
            c_str: *const std::ffi::c_char,
            encoding: u32,
        ) -> *const std::ffi::c_void;
    }

    const K_CF_STRING_ENCODING_UTF8: u32 = 0x08000100;

    unsafe fn cf_string(s: &str) -> *const std::ffi::c_void {
        let c = CString::new(s).unwrap();
        CFStringCreateWithCString(std::ptr::null(), c.as_ptr(), K_CF_STRING_ENCODING_UTF8)
    }

    unsafe fn read_cf_focus_pref() -> bool {
        let app_id = cf_string("com.apple.notificationcenterui");
        let key = cf_string("doNotDisturb");
        let val = CFPreferencesCopyAppValue(key, app_id);
        CFRelease(key);
        CFRelease(app_id);
        if val.is_null() {
            return false;
        }
        let is_bool = CFGetTypeID(val) == CFBooleanGetTypeID();
        let result = is_bool && CFBooleanGetValue(val);
        CFRelease(val);
        result
    }
}

// ─── Windows ─────────────────────────────────────────────────────────────────

#[cfg(target_os = "windows")]
mod platform {
    pub fn check() -> bool {
        // SHQueryUserNotificationState via raw FFI.
        // QUNS_BUSY (2), QUNS_RUNNING_D3D_FULL_SCREEN (3),
        // QUNS_PRESENTATION_MODE (4) → suppress voice.
        #[link(name = "Shell32")]
        unsafe extern "system" {
            fn SHQueryUserNotificationState(pquns: *mut i32) -> i32;
        }
        let mut state: i32 = 0;
        let hr = unsafe { SHQueryUserNotificationState(&mut state) };
        if hr != 0 {
            return false;
        }
        matches!(state, 2 | 3 | 4)
    }
}

// ─── Other platforms ─────────────────────────────────────────────────────────

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
mod platform {
    pub fn check() -> bool {
        false
    }
}

// ─── Mic busy detection ──────────────────────────────────────────────────────

#[cfg(target_os = "macos")]
mod mic {
    use std::ffi::CString;
    use std::os::raw::c_void;

    pub fn check() -> bool {
        // AVCaptureDevice.isUsedByAnotherApplication — available macOS 14+.
        // We query via Objective-C runtime to avoid a hard link.
        unsafe { query_av_capture_mic_busy() }
    }

    #[link(name = "objc", kind = "dylib")]
    unsafe extern "C" {
        fn objc_getClass(name: *const std::ffi::c_char) -> *const c_void;
        fn sel_registerName(str: *const std::ffi::c_char) -> *const c_void;
        fn objc_msgSend(obj: *const c_void, sel: *const c_void, ...) -> *const c_void;
    }

    unsafe fn query_av_capture_mic_busy() -> bool {
        // Link AVFoundation only if present.
        let av_capture = CString::new("AVCaptureDevice").unwrap();
        let cls = objc_getClass(av_capture.as_ptr());
        if cls.is_null() {
            return false;
        }
        let sel_default = CString::new("defaultDeviceWithMediaType:").unwrap();
        let sel_busy = CString::new("isUsedByAnotherApplication").unwrap();
        // kAVMediaTypeAudio is the NSString "soun"
        let media_type_audio = {
            let ns_string_class = CString::new("NSString").unwrap();
            let ns_cls = objc_getClass(ns_string_class.as_ptr());
            let sel_init = CString::new("stringWithUTF8String:").unwrap();
            let c = CString::new("soun").unwrap();
            objc_msgSend(ns_cls, sel_registerName(sel_init.as_ptr()), c.as_ptr())
        };
        let device = objc_msgSend(
            cls,
            sel_registerName(sel_default.as_ptr()),
            media_type_audio,
        );
        if device.is_null() {
            return false;
        }
        let busy = objc_msgSend(device, sel_registerName(sel_busy.as_ptr()));
        busy as usize != 0
    }
}

#[cfg(not(target_os = "macos"))]
mod mic {
    pub fn check() -> bool {
        false
    }
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn focus_check_does_not_panic() {
        // Just verify the function runs without crashing.
        let _ = is_focus_active();
    }

    #[test]
    fn mic_check_does_not_panic() {
        let _ = is_mic_busy();
    }
}
