//! The last messages and commands the applier handled, for crash reports. Summaries only: ids,
//! counts and statuses, no names.

use std::fmt::Write as _;

use dfdu_scan::{ChunkStatus, ListingStatus, ScanMsg};

use crate::applier::Command;

/// Entries kept.
pub const RECENT: usize = 1000;

/// A count for a summary, saturating.
fn count(n: usize) -> u32 {
    u32::try_from(n).unwrap_or(u32::MAX)
}

#[derive(Clone, Debug)]
pub enum Event {
    Chunk {
        job: u32,
        dir: u32,
        seq: u32,
        files: u32,
        subdirs: u32,
        errors: u32,
        status: &'static str,
    },
    Close {
        job: u32,
        dir: u32,
        chunks: u32,
        status: ListingStatus,
    },
    Stalled {
        job: u32,
        dir: u32,
    },
    Quiescent {
        submitted_seq: u64,
    },
    Command(&'static str),
}

impl Event {
    #[must_use]
    pub fn of_msg(msg: &ScanMsg) -> Event {
        match msg {
            ScanMsg::Chunk(c) => Event::Chunk {
                job: c.job.0,
                dir: c.dir.0,
                seq: c.seq,
                files: count(c.files.len()),
                subdirs: count(c.subdirs.len()),
                errors: count(c.errors.len()),
                status: match c.status {
                    ChunkStatus::Ok => "ok",
                    ChunkStatus::Panicked(_) => "panicked",
                    ChunkStatus::Cancelled => "cancelled",
                },
            },
            ScanMsg::Close {
                job,
                dir,
                chunks,
                status,
                ..
            } => Event::Close {
                job: job.0,
                dir: dir.0,
                chunks: *chunks,
                status: status.clone(),
            },
            ScanMsg::Stalled { job, dir } => Event::Stalled {
                job: job.0,
                dir: dir.0,
            },
            ScanMsg::Quiescent { submitted_seq } => Event::Quiescent {
                submitted_seq: *submitted_seq,
            },
        }
    }

    #[must_use]
    pub fn of_command(cmd: &Command) -> Event {
        Event::Command(match cmd {
            Command::Scan { .. } => "Scan",
            Command::Rescan { .. } => "Rescan",
            Command::Enter { .. } => "Enter",
            Command::Resume { .. } => "Resume",
            Command::Cancel { .. } => "Cancel",
            Command::CancelAll => "CancelAll",
            Command::Delete { .. } => "Delete",
            Command::Undo { .. } => "Undo",
            Command::CommitStaged => "CommitStaged",
            Command::Estimate { .. } => "Estimate",
            Command::RunNative { .. } => "RunNative",
            Command::AbortDelete => "AbortDelete",
            #[cfg(feature = "fault-injection")]
            Command::InjectPanic => "InjectPanic",
            Command::Abandon { .. } => "Abandon",
            Command::AbandonAllStalled => "AbandonAllStalled",
            Command::NativeSizes => "NativeSizes",
        })
    }
}

/// A ring of the last [`RECENT`] events.
#[derive(Default)]
pub struct Recent {
    ring: Vec<Event>,
    /// Events recorded so far; the next one goes to `total % RECENT`.
    total: u64,
}

impl Recent {
    pub fn push(&mut self, e: Event) {
        if self.ring.len() < RECENT {
            self.ring.push(e);
        } else {
            let oldest = self.oldest();
            self.ring[oldest] = e;
        }
        self.total += 1;
    }

    /// Where the oldest kept event is once the ring is full, and the next one goes.
    #[expect(
        clippy::cast_possible_truncation,
        reason = "a remainder of RECENT, which is a usize"
    )]
    fn oldest(&self) -> usize {
        (self.total % RECENT as u64) as usize
    }

    /// Events recorded so far, including those no longer kept.
    #[must_use]
    pub fn total(&self) -> u64 {
        self.total
    }

    /// The kept events, oldest first.
    pub fn iter(&self) -> impl Iterator<Item = &Event> {
        let split = if self.ring.len() < RECENT {
            0
        } else {
            self.oldest()
        };
        self.ring[split..].iter().chain(&self.ring[..split])
    }

    /// One line per kept event, oldest first, under a heading.
    #[must_use]
    pub fn report(&self) -> String {
        let mut s = format!(
            "last {} of {} messages and commands, oldest first:\n",
            self.ring.len(),
            self.total
        );
        for e in self.iter() {
            let _ = writeln!(s, "  {e:?}");
        }
        s
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_the_last_entries_in_order() {
        let mut r = Recent::default();
        for i in 0..2500u64 {
            r.push(Event::Quiescent { submitted_seq: i });
        }
        let seqs: Vec<u64> = r
            .iter()
            .map(|e| match e {
                Event::Quiescent { submitted_seq } => *submitted_seq,
                _ => unreachable!(),
            })
            .collect();
        assert_eq!(seqs, (1500..2500).collect::<Vec<_>>());
        assert_eq!(r.total(), 2500);
        assert!(r.report().starts_with("last 1000 of 2500 "));
    }
}
