//! The last messages and commands the applier handled, for crash reports. Summaries only: ids,
//! counts and statuses, no names.

use std::fmt::Write as _;

use dfdu_scan::{ChunkStatus, ListingStatus, ScanMsg};

use crate::applier::Command;

/// Entries kept.
pub const RECENT: usize = 1000;

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
    pub fn of_msg(msg: &ScanMsg) -> Event {
        match msg {
            ScanMsg::Chunk(c) => Event::Chunk {
                job: c.job.0,
                dir: c.dir.0,
                seq: c.seq,
                files: c.files.len() as u32,
                subdirs: c.subdirs.len() as u32,
                errors: c.errors.len() as u32,
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

    pub fn of_command(cmd: &Command) -> Event {
        Event::Command(match cmd {
            Command::Scan { .. } => "Scan",
            Command::Rescan { .. } => "Rescan",
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
            self.ring[(self.total % RECENT as u64) as usize] = e;
        }
        self.total += 1;
    }

    /// Events recorded so far, including those no longer kept.
    pub fn total(&self) -> u64 {
        self.total
    }

    /// The kept events, oldest first.
    pub fn iter(&self) -> impl Iterator<Item = &Event> {
        let split = if self.ring.len() < RECENT {
            0
        } else {
            (self.total % RECENT as u64) as usize
        };
        self.ring[split..].iter().chain(&self.ring[..split])
    }

    /// One line per kept event, oldest first, under a heading.
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
