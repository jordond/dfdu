//! The helper's registration with launchd, through `SMAppService`: the daemon's plist lives in
//! the app bundle (`Contents/Library/LaunchDaemons`), [`register`] asks launchd to load it, and
//! an administrator approves it once in System Settings, Login Items (macOS 13+).
//!
//! `SMAppService` reads the plist from the bundle of the running app, so these calls only mean
//! something from inside a signed `dfdu.app`; elsewhere the status is [`Status::NotFound`]. It
//! is new in macOS 13: before, the status is `NotFound` too and registering is `Unsupported`.

#![allow(unsafe_code)]

use std::io;

use objc2::rc::autoreleasepool;
use objc2::runtime::AnyClass;
use objc2_foundation::NSString;
use objc2_service_management::{SMAppService, SMAppServiceStatus};

/// Where the daemon stands with launchd.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Status {
    /// Never registered, or unregistered since.
    NotRegistered,
    /// Registered and approved: launchd starts it on demand.
    Enabled,
    /// Registered, but an administrator has not approved it in System Settings, or revoked the
    /// approval. [`open_login_items`] takes them there.
    RequiresApproval,
    /// No such plist in the app bundle, or the app is not a bundle.
    NotFound,
}

impl From<SMAppServiceStatus> for Status {
    fn from(s: SMAppServiceStatus) -> Self {
        match s {
            SMAppServiceStatus::NotRegistered => Self::NotRegistered,
            SMAppServiceStatus::Enabled => Self::Enabled,
            SMAppServiceStatus::RequiresApproval => Self::RequiresApproval,
            _ => Self::NotFound,
        }
    }
}

/// The status of the daemon whose plist in `Contents/Library/LaunchDaemons` is named `plist`
/// (`app.dfdu.helper.plist`).
#[must_use]
pub fn status(plist: &str) -> Status {
    if !available() {
        return Status::NotFound;
    }
    autoreleasepool(|_| {
        // SAFETY: `daemonServiceWithPlistName:` takes any string and returns a new object;
        // `status` takes no arguments. Both are thread safe (Apple documents no thread
        // requirement for `SMAppService`).
        unsafe { SMAppService::daemonServiceWithPlistName(&NSString::from_str(plist)).status() }
            .into()
    })
}

/// Registers the daemon with launchd and returns its status after. The first registration
/// leaves it [`Status::RequiresApproval`] until an administrator approves it; `SMAppService`
/// reports that as an error, which is returned here as that status instead. Registering again
/// is how a changed plist or helper binary is picked up.
///
/// # Errors
///
/// What `SMAppService` reports when the daemon is not registered after: an invalid signature or
/// plist, no such plist, launchd refusing it.
pub fn register(plist: &str) -> io::Result<Status> {
    supported()?;
    autoreleasepool(|_| {
        // SAFETY: as in `status`; `registerAndReturnError:` takes no arguments besides the
        // error out-pointer, which the binding manages.
        unsafe {
            let service = SMAppService::daemonServiceWithPlistName(&NSString::from_str(plist));
            let result = service.registerAndReturnError().map_err(|e| Failure {
                domain: e.domain().to_string(),
                code: e.code(),
                message: e.localizedDescription().to_string(),
            });
            registered(result, service.status().into())
        }
    })
}

/// Unregisters the daemon: launchd stops it and no longer starts it. The approval goes with it.
///
/// # Errors
///
/// What `SMAppService` reports, such as the daemon not being registered.
pub fn unregister(plist: &str) -> io::Result<()> {
    supported()?;
    autoreleasepool(|_| {
        // SAFETY: as in `register`.
        unsafe {
            SMAppService::daemonServiceWithPlistName(&NSString::from_str(plist))
                .unregisterAndReturnError()
        }
        .map_err(|e| {
            io::Error::other(Failure {
                domain: e.domain().to_string(),
                code: e.code(),
                message: e.localizedDescription().to_string(),
            })
        })
    })
}

/// Opens System Settings at Login Items, where the daemon is approved. Does nothing before
/// macOS 13.
pub fn open_login_items() {
    if available() {
        // SAFETY: a class method without arguments.
        unsafe { SMAppService::openSystemSettingsLoginItems() }
    }
}

/// Whether this macOS has `SMAppService` (13+); the binding's class lookup panics otherwise.
fn available() -> bool {
    AnyClass::get(c"SMAppService").is_some()
}

fn supported() -> io::Result<()> {
    if available() {
        Ok(())
    } else {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "the helper needs macOS 13 or later",
        ))
    }
}

/// An `NSError` from `SMAppService`, copied out.
#[derive(Debug)]
pub(super) struct Failure {
    pub(super) domain: String,
    pub(super) code: isize,
    pub(super) message: String,
}

impl std::fmt::Display for Failure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} ({} {})", self.message, self.domain, self.code)
    }
}

impl std::error::Error for Failure {}

/// The value of `SMAppServiceErrorDomain`, the domain of `SMAppService`'s own errors.
const DOMAIN: &str = "SMAppServiceErrorDomain";

/// The code of the error the first registration of a daemon fails with ("Operation not
/// permitted"), leaving it waiting for approval.
const NEEDS_APPROVAL: isize = 1;

/// `kSMErrorAlreadyRegistered`.
const ALREADY_REGISTERED: isize = 12;

/// What [`register`] returns, from what `registerAndReturnError:` said and the status after.
/// The error of a first registration, when it leaves the daemon waiting for approval, is that
/// status, and registering again a daemon that is enabled is that status too. Every other
/// error is one, whatever the status after.
pub(super) fn registered(result: Result<(), Failure>, after: Status) -> io::Result<Status> {
    match result {
        Ok(()) => Ok(after),
        Err(f)
            if after == Status::RequiresApproval
                && f.domain == DOMAIN
                && f.code == NEEDS_APPROVAL =>
        {
            Ok(after)
        }
        Err(f) if after == Status::Enabled && f.code == ALREADY_REGISTERED => Ok(after),
        Err(f) => Err(io::Error::other(f)),
    }
}
