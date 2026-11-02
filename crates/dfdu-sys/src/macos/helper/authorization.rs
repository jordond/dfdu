//! The administrator's authorization each scan as root needs, with Authorization Services: a
//! right of dfdu's own in the system's policy database, granted to an administrator who
//! authenticates (password or Touch ID, in the system's sheet), and valid for a while after.
//!
//! The window obtains it ([`Authorization::obtain`]) and sends its external form, 32 bytes that
//! name it inside `authd`, with the socket. The helper turns those bytes back into an
//! authorization and asks for the right again without interaction ([`check`]), so only an
//! authorization an administrator just granted passes; the rule does not allow root by itself,
//! or the helper (root) would always pass.
//!
//! The right is defined by the helper at start, as root may without asking (the
//! `config.modify.` rule), when it is missing or differs ([`define`]). The window adds it
//! when it is missing too, as anyone may (`config.add.`), so the first prompt already shows
//! dfdu's text.

#![allow(unsafe_code)]

use std::ffi::{CStr, CString, c_char};
use std::io;
use std::ptr::{NonNull, null, null_mut};
use std::time::Duration;

use objc2_core_foundation::{
    CFBoolean, CFDictionary, CFNumber, CFRetained, CFString, CFType, ConcreteType, Type,
};
use objc2_security::{
    AuthorizationCopyRights, AuthorizationCreate, AuthorizationCreateFromExternalForm,
    AuthorizationExternalForm, AuthorizationFlags, AuthorizationFree, AuthorizationItem,
    AuthorizationMakeExternalForm, AuthorizationRef, AuthorizationRightGet, AuthorizationRightSet,
    AuthorizationRights, errAuthorizationCanceled, errAuthorizationDenied,
    errAuthorizationInteractionNotAllowed,
};

/// The size of an authorization's external form.
pub(super) const FORM: usize = 32;

/// A right of dfdu's in the policy database.
#[derive(Copy, Clone, Debug)]
pub struct Right<'a> {
    /// Its name (`app.dfdu.scan-as-root`).
    pub name: &'a str,
    /// What the system's sheet says it is for.
    pub prompt: &'a str,
    /// How long an administrator's authentication stays valid for it, so a rescan within it
    /// does not ask again.
    pub timeout: Duration,
}

/// An authorization of this process, freed on drop. Keep it for as long as rescans should not
/// ask again: the administrator's credential lives in it, for the right's timeout.
#[derive(Debug)]
pub struct Authorization {
    auth: AuthorizationRef,
    right: CString,
}

// SAFETY: an `AuthorizationRef` is a handle to state `authd` keeps, which each call reaches
// through the Security framework's client; it is not tied to the thread that made it. It is
// not `Sync`: calls on one authorization are not documented as safe from two threads at once.
unsafe impl Send for Authorization {}

impl Authorization {
    /// Asks for `right` (adding its definition when the system has none): shows the system's
    /// sheet unless an administrator's credential for it is still valid. Blocks until the user
    /// answers.
    ///
    /// # Errors
    ///
    /// [`is_cancelled`] when the user cancels, `PermissionDenied` when they are no
    /// administrator or cannot authenticate, and other Authorization Services failures.
    pub fn obtain(right: &Right<'_>) -> io::Result<Self> {
        // The helper defines it on its first start anyway; failing here only means the first
        // sheet shows the system's text.
        let name = cstring(right.name)?;
        if definition(&name).is_none() {
            let _ = set(&name, right);
        }
        let a = Self::create(name)?;
        a.confirm()?;
        Ok(a)
    }

    /// An authorization for `right` that has not asked for it yet.
    pub(super) fn create(right: CString) -> io::Result<Self> {
        let mut auth: AuthorizationRef = null();
        // SAFETY: no rights or environment (NULL), and `auth` is valid for the write; the
        // authorization made is ours, freed on drop.
        let s =
            unsafe { AuthorizationCreate(null(), null(), AuthorizationFlags::Defaults, &mut auth) };
        status(s, "creating an authorization")?;
        Ok(Self { auth, right })
    }

    /// Asks for the right again, as [`Authorization::obtain`] does: without a sheet while the
    /// credential is valid. Call it before each scan.
    ///
    /// # Errors
    ///
    /// As for [`Authorization::obtain`].
    pub fn confirm(&self) -> io::Result<()> {
        copy_right(
            self.auth,
            &self.right,
            AuthorizationFlags::InteractionAllowed
                | AuthorizationFlags::ExtendRights
                | AuthorizationFlags::PreAuthorize,
        )
    }

    /// The external form, for the helper.
    pub(super) fn external_form(&self) -> io::Result<[u8; FORM]> {
        let mut form = AuthorizationExternalForm { bytes: [0; FORM] };
        // SAFETY: `self.auth` is live; `form` is valid for the write of its 32 bytes.
        let s = unsafe { AuthorizationMakeExternalForm(self.auth, NonNull::from(&mut form)) };
        status(s, "passing the authorization on")?;
        Ok(form.bytes.map(c_char::cast_unsigned))
    }
}

impl Drop for Authorization {
    fn drop(&mut self) {
        free(self.auth);
    }
}

/// The user cancelled the system's sheet; see [`is_cancelled`].
#[derive(Debug)]
struct Cancelled;

impl std::fmt::Display for Cancelled {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("cancelled")
    }
}

impl std::error::Error for Cancelled {}

/// Whether `e` is the user cancelling the system's sheet, rather than a failure.
#[must_use]
pub fn is_cancelled(e: &io::Error) -> bool {
    e.get_ref().is_some_and(|e| e.is::<Cancelled>())
}

/// The helper's check of the authorization a client sent (its external form, if any): that it
/// holds `right` now, asked for without interaction.
pub(super) fn check(form: Option<&[u8]>, right: &CStr) -> Result<(), String> {
    let form = form.ok_or("the request has no authorization")?;
    let bytes = <[u8; FORM]>::try_from(form).map_err(|_| "the authorization is malformed")?;
    let mut form = AuthorizationExternalForm {
        bytes: bytes.map(u8::cast_signed),
    };
    let mut auth: AuthorizationRef = null();
    // SAFETY: `form` is a valid external form to read (32 bytes; `authd` rejects ones that
    // name nothing) and `auth` valid for the write. The authorization made is ours.
    let s = unsafe {
        AuthorizationCreateFromExternalForm(NonNull::from(&mut form), NonNull::from(&mut auth))
    };
    if s != 0 || auth.is_null() {
        return Err("the authorization is not valid".to_owned());
    }
    let auth = Authorization {
        auth,
        right: right.to_owned(),
    };
    match copy_right(auth.auth, right, AuthorizationFlags::ExtendRights) {
        Ok(()) => Ok(()),
        Err(e) => Err(format!("not authorized to scan as an administrator: {e}")),
    }
}

/// Defines `right` in the policy database when it is missing or differs. Run by the helper
/// at start; leaves the database alone when not root (a helper run by hand, or a test), as
/// only root may change a definition without asking.
pub(super) fn define(right: &Right<'_>) -> io::Result<()> {
    if !rustix::process::geteuid().is_root() {
        return Ok(());
    }
    let name = cstring(right.name)?;
    if definition(&name).is_some_and(|d| matches(&d, right)) {
        return Ok(());
    }
    set(&name, right)
}

/// Writes `right`'s definition, authorized by a new authorization of this process.
fn set(name: &CStr, right: &Right<'_>) -> io::Result<()> {
    let auth = Authorization::create(name.to_owned())?;
    let rule = rule(right);
    let prompt = CFString::from_str(right.prompt);
    // SAFETY: `auth.auth` is live, `name` NUL-terminated, `rule` a dictionary of rule keys,
    // and `prompt` a string; all are only read during the call.
    let s = unsafe {
        AuthorizationRightSet(
            auth.auth,
            NonNull::from(name).cast(),
            rule.as_opaque(),
            Some(&prompt),
            None,
            None,
        )
    };
    status(s, "defining dfdu's right")
}

/// A value in a rule's definition.
enum Value<'a> {
    Str(&'a str),
    Bool(bool),
    Int(i32),
}

/// The definition of `right`, as the policy database's keys: an administrator, who must
/// authenticate (not merely be root or the session's owner), for `timeout`, and only for the
/// authorization that asked (not shared with other processes of the session).
fn entries<'a>(right: &Right<'a>) -> [(&'static str, Value<'a>); 9] {
    let timeout = i32::try_from(right.timeout.as_secs()).unwrap_or(i32::MAX);
    [
        ("class", Value::Str("user")),
        ("group", Value::Str("admin")),
        ("authenticate-user", Value::Bool(true)),
        ("allow-root", Value::Bool(false)),
        ("session-owner", Value::Bool(false)),
        ("shared", Value::Bool(false)),
        ("timeout", Value::Int(timeout)),
        ("version", Value::Int(1)),
        (
            "comment",
            Value::Str("Scanning and deleting as root with dfdu's helper."),
        ),
    ]
}

/// `right`'s definition as a dictionary for `AuthorizationRightSet`.
fn rule(right: &Right<'_>) -> CFRetained<CFDictionary<CFString, CFType>> {
    let entries = entries(right);
    let keys: Vec<_> = entries.iter().map(|(k, _)| CFString::from_str(k)).collect();
    let values: Vec<CFRetained<CFType>> = entries
        .iter()
        .map(|(_, v)| match *v {
            Value::Str(s) => CFString::from_str(s).into(),
            Value::Bool(b) => CFBoolean::new(b).retain().into(),
            Value::Int(n) => CFNumber::new_i32(n).into(),
        })
        .collect();
    let keys: Vec<&CFString> = keys.iter().map(|k| &**k).collect();
    let values: Vec<&CFType> = values.iter().map(|v| &**v).collect();
    CFDictionary::from_slices(&keys, &values)
}

/// The definition of the right `name` in the policy database, if any.
fn definition(name: &CStr) -> Option<CFRetained<CFDictionary<CFString, CFType>>> {
    let mut d: *const CFDictionary = null();
    // SAFETY: `name` is NUL-terminated and `d` valid for the write; a dictionary returned is
    // ours (+1), taken over by `from_raw`.
    let s = unsafe { AuthorizationRightGet(NonNull::from(name).cast(), &mut d) };
    let d = NonNull::new(d.cast_mut()).filter(|_| s == 0)?;
    // SAFETY: as above; the policy database's definitions have string keys.
    let d = unsafe { CFRetained::from_raw(d) };
    // SAFETY: as above.
    Some(unsafe { CFRetained::cast_unchecked(d) })
}

/// Whether the definition `d` from the policy database is `right`'s.
fn matches(d: &CFDictionary<CFString, CFType>, right: &Right<'_>) -> bool {
    fn get<T: ConcreteType>(d: &CFDictionary<CFString, CFType>, k: &str) -> Option<CFRetained<T>> {
        d.get(&CFString::from_str(k))?.downcast::<T>().ok()
    }
    let same = entries(right).iter().all(|(k, v)| match *v {
        Value::Str(s) => get::<CFString>(d, k).is_some_and(|x| x.to_string() == s),
        Value::Bool(b) => get::<CFBoolean>(d, k).is_some_and(|x| x.as_bool() == b),
        Value::Int(n) => get::<CFNumber>(d, k).and_then(|x| x.as_i64()) == Some(i64::from(n)),
    });
    // The description is kept as the prompt for every language (key "").
    let prompt = get::<CFDictionary>(d, "default-prompt").is_some_and(|p| {
        // SAFETY: the localized prompts are strings keyed by language.
        let p = unsafe { p.cast_unchecked::<CFString, CFType>() };
        get::<CFString>(p, "").is_some_and(|x| x.to_string() == right.prompt)
    });
    same && prompt
}

/// Asks the authorization `auth` (live) for `right` with `flags`.
fn copy_right(auth: AuthorizationRef, right: &CStr, flags: AuthorizationFlags) -> io::Result<()> {
    let mut item = AuthorizationItem {
        name: right.as_ptr(),
        valueLength: 0,
        value: null_mut(),
        flags: 0,
    };
    let mut rights = AuthorizationRights {
        count: 1,
        items: &mut item,
    };
    // SAFETY: `auth` is live; `rights` holds one item naming `right` (NUL-terminated), all
    // valid for the call; no environment, and no copy of the rights granted asked for.
    let s = unsafe {
        AuthorizationCopyRights(auth, NonNull::from(&mut rights), null(), flags, null_mut())
    };
    status(s, "asking for the right")
}

fn free(auth: AuthorizationRef) {
    if !auth.is_null() {
        // SAFETY: an authorization of ours, freed once.
        unsafe { AuthorizationFree(auth, AuthorizationFlags::Defaults) };
    }
}

/// An Authorization Services result as an `io::Result`.
fn status(s: i32, what: &str) -> io::Result<()> {
    const CANCELED: i32 = errAuthorizationCanceled;
    const DENIED: i32 = errAuthorizationDenied;
    const NO_INTERACTION: i32 = errAuthorizationInteractionNotAllowed;
    match s {
        0 => Ok(()),
        CANCELED => Err(io::Error::other(Cancelled)),
        DENIED => Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "an administrator did not authorize it",
        )),
        NO_INTERACTION => Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "an administrator's authorization is needed again",
        )),
        _ => Err(io::Error::other(format!(
            "{what} failed (Authorization Services error {s})"
        ))),
    }
}

fn cstring(s: &str) -> io::Result<CString> {
    CString::new(s).map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "a NUL in a name"))
}

#[cfg(test)]
mod tests {
    use super::*;

    const RIGHT: Right<'static> = Right {
        name: "app.dfdu.test",
        prompt: "dfdu wants to test.",
        timeout: Duration::from_secs(300),
    };

    /// The definition as the policy database returns it: the rule's keys, and the prompt for
    /// every language.
    fn stored(right: &Right<'_>) -> CFRetained<CFDictionary<CFString, CFType>> {
        let rule = rule(right);
        let (keys, values) = rule.to_vecs();
        let prompt: CFRetained<CFDictionary<CFString, CFString>> = CFDictionary::from_slices(
            &[&*CFString::from_str("")],
            &[&*CFString::from_str(right.prompt)],
        );
        let mut keys: Vec<&CFString> = keys.iter().map(|k| &**k).collect();
        let mut values: Vec<&CFType> = values.iter().map(|v| &**v).collect();
        let key = CFString::from_str("default-prompt");
        keys.push(&key);
        values.push(prompt.as_opaque());
        CFDictionary::from_slices(&keys, &values)
    }

    #[test]
    fn a_definition_matches_only_the_same_right() {
        assert!(matches(&stored(&RIGHT), &RIGHT));
        let longer = Right {
            timeout: Duration::from_secs(600),
            ..RIGHT
        };
        assert!(!matches(&stored(&longer), &RIGHT));
        let other = Right {
            prompt: "something else",
            ..RIGHT
        };
        assert!(!matches(&stored(&other), &RIGHT));
        assert!(!matches(&rule(&RIGHT), &RIGHT), "no prompt");
    }

    #[test]
    fn garbage_is_not_an_authorization() {
        let right = c"config.add.app.dfdu.test";
        assert!(check(None, right).is_err());
        assert!(check(Some(&[7; 31]), right).is_err());
        assert!(check(Some(&[7; FORM]), right).is_err());
    }

    #[test]
    fn cancelled_is_told_apart() {
        assert!(is_cancelled(
            &status(errAuthorizationCanceled, "x").unwrap_err()
        ));
        assert!(!is_cancelled(
            &status(errAuthorizationDenied, "x").unwrap_err()
        ));
    }
}
