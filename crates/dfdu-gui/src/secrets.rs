//! The passwords and key passphrases ssh asked for: kept in memory until the window closes, and
//! in the system's store (Keychain, Credential Manager, the Secret Service) when the user asks
//! to remember one. Never in the settings file. One that the same ssh asks for again was wrong:
//! it is forgotten everywhere ([`Secrets::forget`]) and the user asked, so a wrong password is
//! never sent twice.
//!
//! The system's store blocks (it may ask to be unlocked): it is read ([`Secrets::lookup`]),
//! written and cleared in order on a thread of its own, finished before the window closes, and
//! why a write failed comes back ([`Secrets::warning`]).

use std::collections::HashMap;

use crossbeam_channel::{Receiver, Sender};
use zeroize::Zeroizing;

use crate::askpass::Kind;

/// The service the system's store files them under.
#[cfg_attr(any(not(feature = "keychain"), test), allow(dead_code))]
const SERVICE: &str = "app.dfdu.gui.ssh";

/// What a secret is filed under: `ssh:user@host[:port]` for a password, `key:/path` for a
/// passphrase; `None` for what is never remembered (a host key's answer, a one-time code).
pub fn id(kind: &Kind, port: Option<u16>) -> Option<String> {
    match kind {
        Kind::Password { account } => Some(match port {
            Some(p) => format!("ssh:{account}:{p}"),
            None => format!("ssh:{account}"),
        }),
        Kind::Passphrase { key } => Some(format!("key:{key}")),
        Kind::HostKey | Kind::Other => None,
    }
}

/// The secrets of this run.
pub struct Secrets {
    kept: HashMap<String, Zeroizing<String>>,
    /// The system's store's work, done in order on one thread; `None` without the thread.
    jobs: Option<Sender<Job>>,
    worker: Option<std::thread::JoinHandle<()>>,
    warnings: Receiver<String>,
}

/// Work for the system's store.
enum Job {
    Get {
        id: String,
        found: Sender<Option<Zeroizing<String>>>,
        ctx: egui::Context,
    },
    Set {
        id: String,
        secret: Zeroizing<String>,
    },
    Delete(String),
}

impl Default for Secrets {
    fn default() -> Self {
        let (warn, warnings) = crossbeam_channel::unbounded();
        let (jobs, todo) = crossbeam_channel::unbounded::<Job>();
        let work = move || {
            for job in todo {
                match job {
                    Job::Get { id, found, ctx } => {
                        let _ = found.send(store::get(&id));
                        ctx.request_repaint();
                    }
                    Job::Set { id, secret } => {
                        if let Err(e) = store::set(&id, &secret) {
                            let what = if id.starts_with("key:") {
                                "The key's passphrase"
                            } else {
                                "The password"
                            };
                            let _ = warn.send(format!("{what} was not remembered: {e}"));
                        }
                    }
                    Job::Delete(id) => store::delete(&id),
                }
            }
        };
        let worker = std::thread::Builder::new()
            .name("dfdu-gui-keychain".into())
            .spawn(work)
            .ok();
        Secrets {
            kept: HashMap::new(),
            jobs: worker.is_some().then_some(jobs),
            worker,
            warnings,
        }
    }
}

impl Drop for Secrets {
    /// Finishes what the store was asked to do: a wrong password asked to be forgotten is.
    fn drop(&mut self) {
        self.jobs = None;
        if let Some(w) = self.worker.take() {
            let _ = w.join();
        }
    }
}

impl Secrets {
    /// The one kept for `id` in this run.
    pub fn get(&self, id: &str) -> Option<Zeroizing<String>> {
        self.kept.get(id).cloned()
    }

    /// Keeps `secret` for `id` in this run, and in the system's store when `remember`.
    pub fn keep(&mut self, id: &str, secret: Zeroizing<String>, remember: bool) {
        if remember {
            self.job(Job::Set {
                id: id.to_string(),
                secret: secret.clone(),
            });
        }
        self.kept.insert(id.to_string(), secret);
    }

    /// Forgets `id` here and in the system's store.
    pub fn forget(&mut self, id: &str) {
        self.kept.remove(id);
        self.job(Job::Delete(id.to_string()));
    }

    /// Reads `id` from the system's store, after what it was asked before; the answer comes on
    /// the channel (closed without the store), with a frame asked of `ctx`.
    pub fn lookup(&self, id: String, ctx: &egui::Context) -> Receiver<Option<Zeroizing<String>>> {
        let (found, rx) = crossbeam_channel::bounded(1);
        let ctx = ctx.clone();
        self.job(Job::Get { id, found, ctx });
        rx
    }

    fn job(&self, job: Job) {
        if let Some(jobs) = &self.jobs {
            let _ = jobs.send(job);
        }
    }

    /// Why a password could not be remembered, once.
    pub fn warning(&self) -> Option<String> {
        self.warnings.try_recv().ok()
    }
}

/// What the system's store is called here, for the switch that remembers in it; `None`
/// without one.
pub fn store_name() -> Option<&'static str> {
    if !cfg!(feature = "keychain") {
        return None;
    }
    Some(if cfg!(target_os = "macos") {
        "Keychain"
    } else if cfg!(windows) {
        "Credential Manager"
    } else {
        "the keyring"
    })
}

#[cfg(all(feature = "keychain", not(test)))]
mod store {
    use zeroize::Zeroizing;

    use super::SERVICE;

    fn entry(id: &str) -> Result<keyring::Entry, String> {
        keyring::Entry::new(SERVICE, id).map_err(|e| e.to_string())
    }

    pub fn get(id: &str) -> Option<Zeroizing<String>> {
        entry(id).ok()?.get_password().ok().map(Zeroizing::new)
    }

    pub fn set(id: &str, secret: &str) -> Result<(), String> {
        entry(id)?.set_password(secret).map_err(|e| e.to_string())
    }

    pub fn delete(id: &str) {
        if let Ok(e) = entry(id) {
            let _ = e.delete_credential();
        }
    }
}

/// Without the feature there is no store: nothing is remembered between runs.
#[cfg(any(not(feature = "keychain"), test))]
mod store {
    use zeroize::Zeroizing;

    pub fn get(_: &str) -> Option<Zeroizing<String>> {
        None
    }

    pub fn set(_: &str, _: &str) -> Result<(), String> {
        Err("this build cannot remember passwords".into())
    }

    pub fn delete(_: &str) {}
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn passwords_are_filed_by_account_and_port_and_passphrases_by_key() {
        let password = Kind::Password {
            account: "me@nas".into(),
        };
        assert_eq!(id(&password, None).as_deref(), Some("ssh:me@nas"));
        assert_eq!(
            id(&password, Some(2222)).as_deref(),
            Some("ssh:me@nas:2222")
        );
        let key = Kind::Passphrase {
            key: "/k/id".into(),
        };
        assert_eq!(id(&key, Some(22)).as_deref(), Some("key:/k/id"));
        assert_eq!(id(&Kind::HostKey, None), None);
        assert_eq!(id(&Kind::Other, None), None);
    }

    #[test]
    fn kept_for_the_run_until_forgotten() {
        let mut s = Secrets::default();
        assert!(s.get("ssh:me@nas").is_none());
        s.keep("ssh:me@nas", Zeroizing::new("pw".into()), false);
        assert_eq!(
            s.get("ssh:me@nas").as_deref().map(String::as_str),
            Some("pw")
        );
        s.forget("ssh:me@nas");
        assert!(s.get("ssh:me@nas").is_none());
    }
}
