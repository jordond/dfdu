//! `dfdu recover`: what earlier sessions left unfinished (staged items, interrupted deletes),
//! and finishing, restoring or dismissing it. Finishing and restoring are a dry run unless
//! `--yes` is given, as for `dfdu clean --finish-staged`; dismissing changes nothing on disk.

use std::path::PathBuf;
use std::process::ExitCode;

use clap::Args;
use dfdu_clean::recover::{Pending, Unfinished};

use crate::{EXIT_DELETE_FAILED, print_out};

#[derive(Args)]
pub struct RecoverArgs {
    /// Delete what is left of these staged items (numbers from the list).
    #[arg(long, value_name = "N", value_delimiter = ',')]
    finish: Vec<usize>,
    /// Put these staged items back at their paths (next to them as NAME.restored-N if
    /// something is there now), or take these items found in the trash back out of it.
    #[arg(long, value_name = "N", value_delimiter = ',')]
    restore: Vec<usize>,
    /// Close these interrupted items: nothing is done on disk.
    #[arg(long, value_name = "N", value_delimiter = ',')]
    dismiss: Vec<usize>,
    /// Finish and restore. Without it nothing is changed: what would be done is printed.
    #[arg(long)]
    yes: bool,
    /// The journal directory (the default is dfdu's state directory).
    #[arg(long, hide = true)]
    journals: Option<PathBuf>,
}

/// `dfdu recover`: lists the unfinished items, numbered from 1, then does what was asked
/// for each, highest number first so the numbers stay valid.
pub fn run(args: RecoverArgs) -> Result<ExitCode, ExitCode> {
    let Some(dir) = args.journals.or_else(dfdu_sys::dirs::journal_dir) else {
        eprintln!("dfdu: no state directory for the deletion journal");
        return Err(ExitCode::from(1));
    };
    let mut r = match dfdu_clean::recover::scan(&dir) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("dfdu: {}: {e}", dir.display());
            return Err(ExitCode::from(1));
        }
    };
    for n in &r.notes {
        eprintln!("dfdu: {n}");
    }
    let mut todo: Vec<(usize, &str)> = args
        .finish
        .iter()
        .map(|&n| (n, "finish"))
        .chain(args.restore.iter().map(|&n| (n, "restore")))
        .chain(args.dismiss.iter().map(|&n| (n, "dismiss")))
        .collect();
    if todo.is_empty() {
        print_out(|w| {
            if r.items.is_empty() {
                return writeln!(w, "nothing unfinished");
            }
            for (i, u) in r.items.iter().enumerate() {
                writeln!(w, "{:>3}  {}", i + 1, u.describe())?;
            }
            Ok(())
        })?;
        return Ok(ExitCode::SUCCESS);
    }
    todo.sort_by_key(|&(n, _)| std::cmp::Reverse(n));
    if todo.windows(2).any(|w| w[0].0 == w[1].0) {
        eprintln!("dfdu: an item is named twice");
        return Err(ExitCode::from(2));
    }
    let mut failed = false;
    let mut dry = false;
    for (n, what) in todo {
        let Some(i) = n.checked_sub(1).filter(|&i| i < r.items.len()) else {
            eprintln!("dfdu: no item {n}");
            failed = true;
            continue;
        };
        let path = r.items[i].path.display().to_string();
        let res = match what {
            "finish" | "restore" if !args.yes => {
                dry = true;
                would(&r.items[i], what == "finish").map(|w| format!("{path}: would be {w}"))
            }
            "finish" => r.finish(i).map(|()| format!("{path}: deleted")),
            "restore" => r
                .restore(i)
                .map(|to| format!("{path}: restored at {}", to.display())),
            _ => r.dismiss(i).map(|()| format!("{path}: dismissed")),
        };
        match res {
            Ok(msg) => print_out(|w| writeln!(w, "{msg}"))?,
            Err(e) => {
                eprintln!("dfdu: {path}: {e}");
                failed = true;
            }
        }
    }
    if dry {
        eprintln!("dfdu: dry run: nothing was deleted or restored (--yes does it)");
    }
    Ok(if failed {
        ExitCode::from(EXIT_DELETE_FAILED)
    } else {
        ExitCode::SUCCESS
    })
}

/// What finishing (or restoring) `u` would do, or why it would be refused, without touching
/// it: the state checks of [`dfdu_clean::recover::Recovery`], for a dry run.
fn would(u: &Unfinished, finish: bool) -> std::io::Result<&'static str> {
    let refused = |why| Err(std::io::Error::other(why));
    match (&u.state, finish) {
        (Pending::Staged { .. } | Pending::PartlyDeleted { .. }, true) => Ok("deleted"),
        (Pending::Staged { .. } | Pending::Trashed, false) => Ok("restored"),
        (Pending::Interrupted { .. }, true) => refused("not staged; nothing to finish"),
        (Pending::Trashed, true) => refused("it is in the trash: empty the trash to delete it"),
        (Pending::PartlyDeleted { .. }, false) => {
            refused("partly deleted; it can only be finished, not restored")
        }
        (Pending::Interrupted { .. }, false) => refused("not staged; nothing to restore"),
    }
}
