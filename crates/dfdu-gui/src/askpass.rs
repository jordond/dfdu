//! ssh's questions, asked in the window: ssh runs this program as its askpass helper
//! (`SSH_ASKPASS`, with `SSH_ASKPASS_REQUIRE=force` so it never asks on a terminal), which
//! passes the question to the window over a socket on the loopback address and prints the
//! answer for ssh ([`client`]). The window listens while it connects ([`Askpass`]) and answers
//! only a helper that brings the token it gave ssh, so no other program can ask in its name.
//!
//! The helper sends `DFDU-ASKPASS 2 <token> <ssh's pid> <ask|confirm>`, a newline and the
//! question, then closes its side; the window answers `OK`, a newline and the answer, or `NO`
//! when the question was cancelled. A notice that needs no answer (`SSH_ASKPASS_PROMPT=none`,
//! a security key's "touch it") is not passed on. Needs OpenSSH 8.4 (Win32-OpenSSH 8.6) for
//! `SSH_ASKPASS_REQUIRE`.

use std::ffi::OsString;
use std::io::{self, Read, Write};
use std::net::{Shutdown, SocketAddr, TcpListener, TcpStream};
use std::process::ExitCode;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use crossbeam_channel::{Receiver, Sender};
use zeroize::Zeroizing;

/// Where the helper finds the window: its address, in ssh's environment.
pub const ADDR_VAR: &str = "DFDU_ASKPASS";
/// The token the helper brings.
pub const TOKEN_VAR: &str = "DFDU_ASKPASS_TOKEN";
const HELLO: &str = "DFDU-ASKPASS 2 ";
/// The longest question taken in.
const MAX_QUESTION: u64 = 8 << 10;
/// How long a helper may take to send its question.
const SEND_TIMEOUT: Duration = Duration::from_secs(10);

/// A question from ssh, to answer once.
pub struct Ask {
    pub prompt: String,
    /// The ssh that asks (its process id; 0 where it is not known): one connection may run
    /// several, such as through a jump host.
    pub from: u32,
    /// A yes or no question (`SSH_ASKPASS_PROMPT=confirm`): any answer is yes.
    pub confirm: bool,
    reply: Sender<Option<Zeroizing<String>>>,
}

impl Ask {
    /// A question as the window gets it, and where its answer goes (tests).
    #[cfg(test)]
    pub fn new(prompt: &str, from: u32) -> (Ask, Receiver<Option<Zeroizing<String>>>) {
        let (reply, answer) = crossbeam_channel::bounded(1);
        let ask = Ask {
            prompt: prompt.into(),
            from,
            confirm: false,
            reply,
        };
        (ask, answer)
    }

    /// Answers with `answer`, or cancels with `None`; a question dropped unanswered is
    /// cancelled.
    pub fn answer(self, answer: Option<Zeroizing<String>>) {
        let _ = self.reply.send(answer);
    }
}

/// The window's end, listening while a connection is made; stops when dropped.
pub struct Askpass {
    addr: SocketAddr,
    token: Zeroizing<String>,
    asks: Receiver<Ask>,
    stop: Arc<AtomicBool>,
}

impl Askpass {
    /// Listens on the loopback address, asking `ctx` for a frame when a question comes.
    ///
    /// # Errors
    ///
    /// No socket, or no random token.
    pub fn start(ctx: &egui::Context) -> io::Result<Askpass> {
        let listener = TcpListener::bind(("127.0.0.1", 0))?;
        let addr = listener.local_addr()?;
        let token = Zeroizing::new(token()?);
        let (tx, asks) = crossbeam_channel::unbounded();
        let stop = Arc::new(AtomicBool::new(false));
        let (want, stopped, ctx) = (token.clone(), stop.clone(), ctx.clone());
        std::thread::Builder::new()
            .name("dfdu-gui-askpass".into())
            .spawn(move || {
                for conn in listener.incoming() {
                    if stopped.load(Ordering::SeqCst) {
                        break;
                    }
                    let Ok(conn) = conn else { continue };
                    let (want, tx, ctx) = (want.clone(), tx.clone(), ctx.clone());
                    // One thread a question: ssh asks one at a time.
                    let _ = std::thread::Builder::new()
                        .name("dfdu-gui-askpass-question".into())
                        .spawn(move || {
                            let _ = serve(conn, &want, &tx, &ctx);
                        });
                }
            })?;
        Ok(Askpass {
            addr,
            token,
            asks,
            stop,
        })
    }

    /// The environment that makes ssh ask through `helper` (this program) and the window.
    pub fn env(&self, helper: OsString) -> Vec<(OsString, OsString)> {
        let mut env = vec![
            ("SSH_ASKPASS".into(), helper),
            ("SSH_ASKPASS_REQUIRE".into(), "force".into()),
            (ADDR_VAR.into(), self.addr.to_string().into()),
            (TOKEN_VAR.into(), self.token.as_str().into()),
        ];
        // OpenSSH before 8.4 uses askpass only with a display, and ignores the requirement.
        if std::env::var_os("DISPLAY").is_none_or(|d| d.is_empty()) {
            env.push(("DISPLAY".into(), ":0".into()));
        }
        env
    }

    /// The next question, when one came.
    pub fn next(&self) -> Option<Ask> {
        self.asks.try_recv().ok()
    }
}

impl Drop for Askpass {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        // Wakes the listener to see it.
        let _ = TcpStream::connect_timeout(&self.addr, Duration::from_secs(1));
    }
}

/// 32 random bytes, in hex.
fn token() -> io::Result<String> {
    let mut bytes = [0u8; 32];
    getrandom::fill(&mut bytes).map_err(|e| io::Error::other(e.to_string()))?;
    Ok(hex(&bytes))
}

fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    bytes.iter().fold(String::new(), |mut s, b| {
        let _ = write!(s, "{b:02x}");
        s
    })
}

/// Whether `a` and `b` are equal, in time that does not depend on where they differ.
fn same(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |d, (x, y)| d | (x ^ y)) == 0
}

/// Takes one question from `conn`, passes it on, and writes the answer back.
fn serve(mut conn: TcpStream, want: &str, tx: &Sender<Ask>, ctx: &egui::Context) -> io::Result<()> {
    conn.set_read_timeout(Some(SEND_TIMEOUT))?;
    let mut sent = Vec::new();
    (&mut conn).take(MAX_QUESTION).read_to_end(&mut sent)?;
    let sent = String::from_utf8_lossy(&sent);
    let Some((hello, prompt)) = sent.split_once('\n') else {
        return Ok(());
    };
    let Some(rest) = hello.strip_prefix(HELLO) else {
        return Ok(());
    };
    let mut words = rest.split_whitespace();
    let (Some(token), from, mode) = (words.next(), words.next(), words.next()) else {
        return Ok(());
    };
    if !same(token.as_bytes(), want.as_bytes()) {
        return Ok(());
    }
    let from = from.and_then(|f| f.parse().ok()).unwrap_or(0);
    let confirm = mode == Some("confirm");
    let (reply, answer) = crossbeam_channel::bounded(1);
    let ask = Ask {
        prompt: prompt.trim_end().to_string(),
        from,
        confirm,
        reply,
    };
    if tx.send(ask).is_err() {
        return conn.write_all(b"NO\n");
    }
    ctx.request_repaint();
    match answer.recv().ok().flatten() {
        Some(a) => {
            let mut out = Zeroizing::new(Vec::with_capacity(a.len() + 3));
            out.extend_from_slice(b"OK\n");
            out.extend_from_slice(a.as_bytes());
            conn.write_all(&out)
        }
        None => conn.write_all(b"NO\n"),
    }
}

/// Whether this program was started by ssh as its askpass helper: the window's address and
/// token are in the environment.
pub fn is_helper() -> bool {
    std::env::var_os(ADDR_VAR).is_some() && std::env::var_os(TOKEN_VAR).is_some()
}

/// Runs as ssh's askpass helper: asks the window `prompt` (the first argument) and prints the
/// answer on standard output; exits 1 when cancelled or the window cannot be reached, which
/// ssh takes as no answer.
pub fn client() -> ExitCode {
    let prompt = std::env::args().nth(1).unwrap_or_default();
    let mode = std::env::var("SSH_ASKPASS_PROMPT").unwrap_or_default();
    // A notice (touch the security key): ssh shows it and kills the helper when done.
    if mode == "none" {
        return ExitCode::SUCCESS;
    }
    let confirm = mode == "confirm";
    match ask(&prompt, confirm) {
        Ok(Some(answer)) => {
            let mut out = io::stdout().lock();
            match out
                .write_all(answer.as_bytes())
                .and_then(|()| out.write_all(b"\n"))
                .and_then(|()| out.flush())
            {
                Ok(()) => ExitCode::SUCCESS,
                Err(_) => ExitCode::from(1),
            }
        }
        Ok(None) => ExitCode::from(1),
        Err(e) => {
            eprintln!("dfdu-gui askpass: {e}");
            ExitCode::from(1)
        }
    }
}

fn ask(prompt: &str, confirm: bool) -> io::Result<Option<Zeroizing<String>>> {
    let var = |name| std::env::var(name).map_err(|_| io::Error::other(format!("no {name}")));
    let addr: SocketAddr = var(ADDR_VAR)?
        .parse()
        .map_err(|_| io::Error::other(format!("{ADDR_VAR} is not an address")))?;
    if !addr.ip().is_loopback() {
        return Err(io::Error::other(format!(
            "{ADDR_VAR} is not on this machine"
        )));
    }
    let token = Zeroizing::new(var(TOKEN_VAR)?);
    ask_at(addr, &token, prompt, ssh_pid(), confirm)
}

/// Asks the window at `addr` `prompt`, with `token`.
fn ask_at(
    addr: SocketAddr,
    token: &str,
    prompt: &str,
    from: u32,
    confirm: bool,
) -> io::Result<Option<Zeroizing<String>>> {
    let mut conn = TcpStream::connect_timeout(&addr, Duration::from_secs(5))?;
    let mode = if confirm { "confirm" } else { "ask" };
    let hello = format!("{HELLO}{token} {from} {mode}\n{prompt}");
    conn.write_all(Zeroizing::new(hello).as_bytes())?;
    conn.shutdown(Shutdown::Write)?;
    let mut reply = Zeroizing::new(Vec::new());
    conn.read_to_end(&mut reply)?;
    Ok(reply
        .strip_prefix(b"OK\n")
        .map(|a| Zeroizing::new(String::from_utf8_lossy(a).into_owned())))
}

/// The ssh that started this helper: its parent.
fn ssh_pid() -> u32 {
    #[cfg(unix)]
    return std::os::unix::process::parent_id();
    #[cfg(not(unix))]
    return 0;
}

/// What ssh asks.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Kind {
    /// The account's password: `user@host's password:`, `(user@host) Password:`.
    Password { account: String },
    /// The passphrase of a private key: `Enter passphrase for key '/path':`.
    Passphrase { key: String },
    /// Whether to trust a host not seen before, or whose key is not known yet.
    HostKey,
    /// Anything else (a one-time code): never remembered.
    Other,
}

/// What `prompt` asks for.
pub fn kind(prompt: &str) -> Kind {
    let p = prompt.trim();
    // ssh's own question, not a server's text (those come as `(user@host) ...`).
    let ends = [
        "continue connecting (yes/no/[fingerprint])?",
        "continue connecting (yes/no)?",
    ];
    if !p.starts_with('(') && ends.iter().any(|e| p.ends_with(e)) {
        return Kind::HostKey;
    }
    if let Some(rest) = p.strip_prefix("Enter passphrase for key '")
        && let Some((key, _)) = rest.rsplit_once('\'')
    {
        return Kind::Passphrase { key: key.into() };
    }
    if let Some(account) = p.strip_suffix("'s password:") {
        return Kind::Password {
            account: account.into(),
        };
    }
    if let Some(rest) = p.strip_prefix('(')
        && let Some((account, ask)) = rest.split_once(") ")
        && ask.eq_ignore_ascii_case("password:")
    {
        return Kind::Password {
            account: account.into(),
        };
    }
    Kind::Other
}

#[cfg(test)]
mod tests;
