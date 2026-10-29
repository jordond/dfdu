//! An engine talking to an agent in a thread, over two pipes.

use dfdu_engine::{Engine, EngineOptions, RemoteIo, RemoteRules};
use dfdu_scan::PortableBackend;
use dfdu_scan::classify::ClassOpts;

pub fn opts() -> EngineOptions {
    EngineOptions {
        workers: 2,
        journal_dir: None,
        check_invariants: true,
        ..Default::default()
    }
}

/// An engine talking to an agent thread; the agent ends when the engine closes its stdin.
pub fn remote() -> (
    Engine<PortableBackend>,
    std::thread::JoinHandle<std::io::Result<()>>,
) {
    let (e, agent, _) = remote_with(std::path::Path::new("/"), None, None);
    (e, agent)
}

/// [`remote`] for scans of `root`, classifying with `rules`, the agent journalling deletions in
/// `journal`; also what the root classifies against.
pub fn remote_with(
    root: &std::path::Path,
    rules: Option<RemoteRules>,
    journal: Option<std::path::PathBuf>,
) -> (
    Engine<PortableBackend>,
    std::thread::JoinHandle<std::io::Result<()>>,
    Option<ClassOpts>,
) {
    let (down_r, down_w) = std::io::pipe().unwrap();
    let (up_r, up_w) = std::io::pipe().unwrap();
    let agent = std::thread::spawn(move || {
        dfdu_engine::agent::run(PortableBackend::default(), 2, journal, down_r, up_w)
    });
    let (e, c) = Engine::start_remote(
        PortableBackend::default(),
        opts(),
        RemoteIo {
            reader: Box::new(up_r),
            writer: Box::new(down_w),
            child: None,
        },
        root,
        rules,
    )
    .unwrap();
    assert!(c.tool.starts_with("dfdu "), "{}", c.tool);
    assert_eq!(c.errors, Vec::<String>::new());
    (e, agent, c.class)
}
