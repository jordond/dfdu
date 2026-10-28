//! Scans on another host: `dfdu agent` scans there, and `--remote` starts it through `ssh`.

use std::process::ExitCode;

use dfdu_engine::{Engine, EngineOptions};
use dfdu_scan::classify::ClassOpts;
use dfdu_scan::{NativeBackend, native_backend};

/// `dfdu agent`: the scanner for a dfdu on another host, over stdin and stdout.
pub fn agent(threads: Option<usize>) -> ExitCode {
    let workers = threads.unwrap_or_else(|| dfdu_engine::workers_for(dfdu_sys::Storage::Unknown));
    let r = dfdu_engine::agent::run(
        native_backend(dfdu_sys::Storage::Unknown, false),
        workers,
        dfdu_clean::default_journal_dir(),
        std::io::stdin(),
        std::io::stdout(),
    );
    match r {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("dfdu agent: {e}");
            ExitCode::from(1)
        }
    }
}

/// Starts `dfdu agent` on `host` through `ssh` (or the command in `DFDU_REMOTE_COMMAND`,
/// split at spaces, which replaces `ssh HOST dfdu agent`).
fn agent_io(host: &str) -> std::io::Result<dfdu_engine::RemoteIo> {
    use std::process::{Command as Proc, Stdio};
    let argv: Vec<String> = match std::env::var("DFDU_REMOTE_COMMAND") {
        Ok(c) => c.split_whitespace().map(str::to_string).collect(),
        Err(_) => ["ssh", "--", host, "dfdu", "agent"]
            .map(str::to_string)
            .to_vec(),
    };
    let (prog, rest) = argv
        .split_first()
        .ok_or_else(|| std::io::Error::other("DFDU_REMOTE_COMMAND is empty"))?;
    let mut child = Proc::new(prog)
        .args(rest)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .map_err(|e| std::io::Error::new(e.kind(), format!("{prog}: {e}")))?;
    let pipe = |what| std::io::Error::other(format!("{prog}: no {what} pipe"));
    let reader = Box::new(child.stdout.take().ok_or_else(|| pipe("stdout"))?);
    let writer = Box::new(child.stdin.take().ok_or_else(|| pipe("stdin"))?);
    Ok(dfdu_engine::RemoteIo {
        reader,
        writer,
        child: Some(child),
    })
}

/// An engine scanning on `host` through its agent, classifying with `rules`, and what the scan
/// root classifies against; reports a failure to start.
pub fn engine(
    host: &str,
    opts: EngineOptions,
    rules: Option<dfdu_engine::RemoteRules>,
) -> Result<(Engine<NativeBackend>, Option<ClassOpts>), ExitCode> {
    let started = agent_io(host).and_then(|io| {
        Engine::start_remote(
            native_backend(dfdu_sys::Storage::Unknown, false),
            opts,
            io,
            rules,
        )
    });
    match started {
        Ok((e, c)) => {
            for msg in c.errors {
                eprintln!("dfdu: {host}: {msg}");
            }
            Ok((e, c.class))
        }
        Err(e) => {
            eprintln!("dfdu: {host}: {e}");
            Err(ExitCode::from(1))
        }
    }
}
