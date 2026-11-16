//! macOS implementation of [`super`]: legacy `SecAccess` / `SecACL` FFI.
//!
//! Every `Copy`/`Create` result is owned and released through [`Owned`].
//! Errors carry the call name and OSStatus, nothing from the item itself.
#![allow(deprecated, non_snake_case)]

use super::{AclError, Trust, decide};
use core_foundation_sys::array::{
    CFArrayAppendValue, CFArrayCreateMutableCopy, CFArrayGetCount, CFArrayGetValueAtIndex,
    CFArrayRef, CFMutableArrayRef,
};
use core_foundation_sys::base::{CFRelease, CFTypeRef, OSStatus};
use core_foundation_sys::data::{CFDataGetBytePtr, CFDataGetLength, CFDataRef};
use core_foundation_sys::string::CFStringRef;
use security_framework_sys::base::{SecAccessRef, SecKeychainItemRef, errSecItemNotFound};
use security_framework_sys::keychain::SecKeychainFindGenericPassword;
use std::ffi::{CString, c_void};
use std::path::Path;

type SecACLRef = *mut c_void;
type SecTrustedApplicationRef = *mut c_void;

#[link(name = "Security", kind = "framework")]
unsafe extern "C" {
    static kSecACLAuthorizationDecrypt: CFStringRef;
    fn SecKeychainItemCopyAccess(item: SecKeychainItemRef, access: *mut SecAccessRef) -> OSStatus;
    fn SecKeychainItemSetAccess(item: SecKeychainItemRef, access: SecAccessRef) -> OSStatus;
    fn SecAccessCopyMatchingACLList(access: SecAccessRef, tag: CFTypeRef) -> CFArrayRef;
    fn SecACLCopyContents(
        acl: SecACLRef,
        apps: *mut CFArrayRef,
        description: *mut CFStringRef,
        prompt: *mut u16,
    ) -> OSStatus;
    fn SecACLSetContents(
        acl: SecACLRef,
        apps: CFArrayRef,
        description: CFStringRef,
        prompt: u16,
    ) -> OSStatus;
    fn SecTrustedApplicationCreateFromPath(
        path: *const std::ffi::c_char,
        app: *mut SecTrustedApplicationRef,
    ) -> OSStatus;
    fn SecTrustedApplicationCopyData(
        app: SecTrustedApplicationRef,
        data: *mut CFDataRef,
    ) -> OSStatus;
}

/// A CF object this code owns (+1); released on drop. Null is allowed.
struct Owned(CFTypeRef);

impl Drop for Owned {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe { CFRelease(self.0) }
        }
    }
}

fn os(call: &'static str, status: OSStatus) -> Result<(), AclError> {
    if status == 0 {
        Ok(())
    } else {
        Err(AclError::Os { call, status })
    }
}

/// The item, found by service and account without reading its secret, so no
/// prompt is raised.
fn find(service: &str, account: &str) -> Result<Owned, AclError> {
    let mut item: SecKeychainItemRef = std::ptr::null_mut();
    let status = unsafe {
        SecKeychainFindGenericPassword(
            std::ptr::null(),
            service.len() as u32,
            service.as_ptr().cast(),
            account.len() as u32,
            account.as_ptr().cast(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            &mut item,
        )
    };
    if status == errSecItemNotFound {
        return Err(AclError::NotFound {
            service: service.into(),
            account: account.into(),
        });
    }
    os("SecKeychainFindGenericPassword", status)?;
    Ok(Owned(item as CFTypeRef))
}

fn trusted_app(path: &Path) -> Result<Owned, AclError> {
    let c = CString::new(path.as_os_str().as_encoded_bytes()).map_err(|_| AclError::Os {
        call: "path",
        status: -50,
    })?;
    let mut app: SecTrustedApplicationRef = std::ptr::null_mut();
    os("SecTrustedApplicationCreateFromPath", unsafe {
        SecTrustedApplicationCreateFromPath(c.as_ptr(), &mut app)
    })?;
    Ok(Owned(app as CFTypeRef))
}

fn app_bytes(app: CFTypeRef) -> Option<Vec<u8>> {
    let mut data: CFDataRef = std::ptr::null();
    let status = unsafe { SecTrustedApplicationCopyData(app as _, &mut data) };
    if status != 0 || data.is_null() {
        return None;
    }
    let _own = Owned(data as CFTypeRef);
    let (ptr, len) = unsafe { (CFDataGetBytePtr(data), CFDataGetLength(data)) };
    Some(unsafe { std::slice::from_raw_parts(ptr, len as usize) }.to_vec())
}

/// One decrypt ACL: the ACL itself (borrowed from `list`), its owned app list
/// (null = any app), description and prompt selector.
struct Acl {
    acl: SecACLRef,
    apps: Owned,
    description: Owned,
    prompt: u16,
}

impl Acl {
    fn app_bytes(&self) -> Option<Vec<Vec<u8>>> {
        let arr = self.apps.0 as CFArrayRef;
        if arr.is_null() {
            return None;
        }
        let n = unsafe { CFArrayGetCount(arr) };
        Some(
            (0..n)
                .filter_map(|i| app_bytes(unsafe { CFArrayGetValueAtIndex(arr, i) }))
                .collect(),
        )
    }
}

/// The item's access record and its decrypt ACLs. `_list` keeps the ACL refs
/// in `acls` alive.
struct Access {
    access: Owned,
    _list: Owned,
    acls: Vec<Acl>,
}

fn access_of(item: &Owned) -> Result<Access, AclError> {
    let mut access: SecAccessRef = std::ptr::null_mut();
    os("SecKeychainItemCopyAccess", unsafe {
        SecKeychainItemCopyAccess(item.0 as _, &mut access)
    })?;
    let access = Owned(access as CFTypeRef);
    let list = unsafe {
        SecAccessCopyMatchingACLList(access.0 as _, kSecACLAuthorizationDecrypt as CFTypeRef)
    };
    let list = Owned(list as CFTypeRef);
    let mut acls = Vec::new();
    if !list.0.is_null() {
        for i in 0..unsafe { CFArrayGetCount(list.0 as CFArrayRef) } {
            let acl = unsafe { CFArrayGetValueAtIndex(list.0 as CFArrayRef, i) } as SecACLRef;
            let mut apps: CFArrayRef = std::ptr::null();
            let mut desc: CFStringRef = std::ptr::null();
            let mut prompt = 0u16;
            os("SecACLCopyContents", unsafe {
                SecACLCopyContents(acl, &mut apps, &mut desc, &mut prompt)
            })?;
            acls.push(Acl {
                acl,
                apps: Owned(apps as CFTypeRef),
                description: Owned(desc as CFTypeRef),
                prompt,
            });
        }
    }
    Ok(Access {
        access,
        _list: list,
        acls,
    })
}

pub(super) fn check(service: &str, account: &str, app: &Path) -> Result<Trust, AclError> {
    let item = find(service, account)?;
    let access = access_of(&item)?;
    let target = trusted_app(app)?;
    let target = app_bytes(target.0).ok_or(AclError::Os {
        call: "SecTrustedApplicationCopyData",
        status: -1,
    })?;
    let lists: Vec<_> = access.acls.iter().map(Acl::app_bytes).collect();
    Ok(decide(&lists, &target))
}

pub(super) fn ensure(service: &str, account: &str, app: &Path) -> Result<bool, AclError> {
    if check(service, account, app)? == Trust::Trusted {
        return Ok(false);
    }
    let item = find(service, account)?;
    let access = access_of(&item)?;
    // `check` said Missing, so every decrypt ACL has an app list.
    let acl = access.acls.first().ok_or(AclError::NoAppList)?;
    if acl.apps.0.is_null() {
        return Err(AclError::NoAppList);
    }
    let target = trusted_app(app)?;
    let apps: CFMutableArrayRef =
        unsafe { CFArrayCreateMutableCopy(std::ptr::null(), 0, acl.apps.0 as CFArrayRef) };
    let _apps = Owned(apps as CFTypeRef);
    unsafe { CFArrayAppendValue(apps, target.0) };
    os("SecACLSetContents", unsafe {
        SecACLSetContents(
            acl.acl,
            apps as CFArrayRef,
            acl.description.0 as _,
            acl.prompt,
        )
    })?;
    // This is the call macOS guards with the keychain password.
    os("SecKeychainItemSetAccess", unsafe {
        SecKeychainItemSetAccess(item.0 as _, access.access.0 as _)
    })?;
    Ok(true)
}
