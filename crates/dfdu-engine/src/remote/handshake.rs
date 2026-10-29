//! Greeting the agent: protocol versions, its host, and the rules both sides classify with.

use std::collections::{HashSet, VecDeque};
use std::io::{self, Read};
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering::SeqCst};
use std::sync::{Arc, OnceLock};

use super::wire::{self, FromAgent, ToAgent, WireClass, read_frame};
use super::{Connected, Link, RemoteIo, RemoteRules};
use dfdu_rules::hash::FastMap as HashMap;
use dfdu_rules::{Env, Platform, RuleSet, Source};
use dfdu_tree::{Host, HostPath};
use parking_lot::Mutex;

/// Greets the agent: the versions must overlap. With `rules`, sends them and builds the set the
/// agent built. Returns the link and the agent's reader.
pub(crate) fn connect(
    io: RemoteIo,
    root: &Path,
    rules: Option<RemoteRules>,
) -> io::Result<(Arc<Link>, Box<dyn Read + Send>, Connected)> {
    let RemoteIo {
        mut reader,
        writer,
        child,
    } = io;
    let mut buf = Vec::new();
    let hello = next_message(
        &mut reader,
        &mut buf,
        io::Error::new(
            io::ErrorKind::UnexpectedEof,
            "the agent closed the link before saying hello",
        ),
    )?;
    let tool = match hello {
        FromAgent::Hello { min, max, tool } if (min..=max).contains(&wire::VERSION) => tool,
        FromAgent::Hello { min, max, tool } => {
            return Err(io::Error::other(format!(
                "{tool} speaks protocol versions {min}-{max}, this dfdu {}",
                wire::VERSION
            )));
        }
        FromAgent::Fatal(s) => return Err(io::Error::other(s)),
        _ => return Err(io::Error::other("the agent did not say hello")),
    };
    // The agent's host is not known until it says; the greeting carries no paths.
    let host = Host::LOCAL;
    let mut link = Link {
        host,
        writer: Mutex::new(writer),
        seq: AtomicU64::new(0),
        pending: Mutex::new(VecDeque::new()),
        pairs: Mutex::new(Vec::new()),
        child: Mutex::new(child),
        dead: AtomicBool::new(false),
        timed_out: AtomicBool::new(false),
        rules: OnceLock::new(),
        lost: Mutex::new(None),
        plans: Mutex::new(HashMap::default()),
        natives: Mutex::new(HashSet::default()),
        measuring: Mutex::new(HashMap::default()),
    };
    link.send(&ToAgent::Hello {
        version: wire::VERSION,
        root: dfdu_tree::name_bytes(root.as_os_str()).into_owned(),
    });
    let (measure_min, privileged, host, root) = loop {
        let closed = io::Error::other("the agent closed the link");
        match next_message(&mut reader, &mut buf, closed)? {
            FromAgent::Heartbeat => {}
            FromAgent::Fatal(s) => return Err(io::Error::other(s)),
            FromAgent::Host {
                measure_min,
                privileged,
                host,
                root,
            } => break (measure_min, privileged, host, root),
            _ => return Err(io::Error::other("the agent did not describe its host")),
        }
    };
    link.host = host;
    let link = Arc::new(link);
    let mut connected = Connected {
        tool,
        host,
        root: HostPath::new(host, root),
        class: None,
        errors: Vec::new(),
        measure_min,
        privileged,
    };
    if let Some(r) = rules {
        exchange_rules(&link, &mut reader, &mut buf, &r, &mut connected)?;
    }
    if link.dead.load(SeqCst) {
        return Err(io::Error::other("the agent closed the link"));
    }
    Ok((link, reader, connected))
}

/// Sends `r` to the agent and builds the set it built from them, into `connected`; rules the
/// two sides do not build alike are left out, with why in `connected.errors`.
fn exchange_rules(
    link: &Link,
    reader: &mut impl Read,
    buf: &mut Vec<u8>,
    r: &RemoteRules,
    connected: &mut Connected,
) -> io::Result<()> {
    link.send(&ToAgent::Rules {
        files: r
            .files
            .iter()
            .map(|(src, text)| {
                let name = match src {
                    Source::Builtin => None,
                    Source::File(p) => Some(p.display().to_string()),
                };
                (name, text.clone())
            })
            .collect(),
        root: connected.root.as_bytes().to_vec(),
    });
    let answer = loop {
        let closed = io::Error::other("the agent closed the link");
        match next_message(reader, buf, closed)? {
            // Before a job starts, batches only say the agent's workers are idle.
            FromAgent::Heartbeat | FromAgent::Batch(_) => {}
            FromAgent::Fatal(s) => return Err(io::Error::other(s)),
            FromAgent::Rules {
                platform,
                vars,
                links,
                fingerprint,
                root,
            } => break (platform, vars, links, fingerprint, root),
            _ => return Err(io::Error::other("the agent did not answer the rules")),
        }
    };
    let (platform, vars, links, fingerprint, root) = answer;
    let built = Built {
        platform: &platform,
        vars: &vars,
        links: &links,
    };
    match agree(&r.files, &built, fingerprint, &root) {
        Ok((set, errors)) => {
            let set = Arc::new(set);
            let _ = link.rules.set(set.clone());
            connected.class = Some(root.with(set));
            connected.errors = errors;
        }
        Err(e) => connected.errors.push(e),
    }
    Ok(())
}

/// The next message from the agent while it is greeted, decoded without rules; `closed` if
/// the link ends first.
fn next_message(
    reader: &mut impl Read,
    buf: &mut Vec<u8>,
    closed: io::Error,
) -> io::Result<FromAgent> {
    let bad = |e: wire::WireError| io::Error::new(io::ErrorKind::InvalidData, e);
    if !read_frame(reader, buf).map_err(bad)? {
        return Err(closed);
    }
    FromAgent::decode(buf, None, Host::LOCAL).map_err(bad)
}

/// What the agent built its rules for ([`FromAgent::Rules`]).
struct Built<'a> {
    platform: &'a str,
    vars: &'a [(String, String)],
    /// The path patterns that lead elsewhere through a link on the agent's disks.
    links: &'a [(String, String)],
}

/// Builds `files` as the agent did, for its platform, variables and links; links on this
/// machine's disks are never looked at, since the paths are the agent's. The set must have the
/// agent's `fingerprint` and name every rule `root` does. Returns the set and its load errors.
fn agree(
    files: &[(Source, String)],
    built: &Built,
    fingerprint: u64,
    root: &WireClass,
) -> Result<(RuleSet, Vec<String>), String> {
    let platform = built.platform;
    let p = Platform::parse(platform).ok_or_else(|| {
        format!("the agent runs on an unknown platform `{platform}`: not classified")
    })?;
    let vars: Vec<(&str, &str)> = (built.vars.iter())
        .map(|(k, v)| (k.as_str(), v.as_str()))
        .collect();
    let env = Env::with(p, &vars).with_links(built.links);
    let (set, errors) = dfdu_rules::load_from(&env, files);
    if set.fingerprint() != fingerprint || !root.fits(&set) {
        return Err(
            "the agent built different rules (another dfdu version?): not classified".into(),
        );
    }
    Ok((
        set,
        errors
            .iter()
            .map(std::string::ToString::to_string)
            .collect(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The set built from the agent's answer must be the agent's: another platform's rules,
    /// or rules the agent does not number alike, classify nothing.
    #[test]
    fn rules_are_agreed_or_dropped() {
        let files = vec![(Source::Builtin, dfdu_rules::load::BUILTIN.to_owned())];
        let vars = vec![("HOME".to_owned(), "/home/u".to_owned())];
        let env = Env::with(Platform::Linux, &[("HOME", "/home/u")]);
        let set = dfdu_rules::load_from(&env, &files).0;
        let root = WireClass {
            path: b"/home/u/p".to_vec(),
            inside: vec![],
            protection: dfdu_rules::protected::State::default(),
            chain: vec![],
        };
        let fp = set.fingerprint();
        let on = |platform| Built {
            platform,
            vars: &vars,
            links: &[],
        };
        let (got, errors) = agree(&files, &on("linux"), fp, &root).unwrap();
        assert_eq!(got.fingerprint(), fp);
        assert!(errors.is_empty(), "{errors:?}");
        // Another platform builds other rules.
        assert_ne!(
            dfdu_rules::load_from(&Env::with(Platform::MacOs, &[]), &files)
                .0
                .fingerprint(),
            fp
        );
        assert!(agree(&files, &on("macos"), fp, &root).is_err());
        assert!(agree(&files, &on("plan9"), fp, &root).is_err());
        assert!(agree(&files, &on("linux"), fp ^ 1, &root).is_err());
        // A path pattern the agent followed through a link: built alike only with the link.
        let links = [(
            "/home/u/.cargo/registry".to_owned(),
            "/mnt/data/cargo/registry".to_owned(),
        )];
        let linked = Built {
            links: &links,
            ..on("linux")
        };
        let agent = dfdu_rules::load_from(&env.clone().with_links(&links), &files).0;
        let lfp = agent.fingerprint();
        assert_ne!(lfp, fp);
        assert!(agree(&files, &on("linux"), lfp, &root).is_err());
        assert_eq!(
            agree(&files, &linked, lfp, &root).unwrap().0.fingerprint(),
            lfp
        );
        let unknown = WireClass {
            inside: vec![dfdu_rules::RuleIx(u16::MAX)],
            ..root
        };
        assert!(agree(&files, &on("linux"), fp, &unknown).is_err());
    }
}
