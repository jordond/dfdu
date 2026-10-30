//! The step of a batch of a resume's check ([`Shared::verify`]): one directory stat'ed per
//! step, under the stall watch.

use std::io;
use std::sync::atomic::Ordering::SeqCst;

use dfdu_tree::Kind;

use super::{StepResult, Worker};
use crate::backend::Backend;
use crate::proto::ScanMsg;
use crate::shared::verify::VerifyWork;
use crate::shared::{Clock, LISTING_PART, Shared, Task};

impl<B: Backend> Worker<B> {
    /// Stats the batch's next directory; after the last, or once its job ended, reports.
    #[expect(
        clippy::expect_used,
        reason = "`step_inner` runs this step only for a running batch task"
    )]
    pub(super) fn verify_step(&mut self, sh: &Shared<B>, clock: &dyn Clock) -> StepResult {
        let r = self.running.as_mut().expect("running");
        let ended = r.job.ended.load(SeqCst);
        let work = r.task.verify.as_mut().expect("batch task");
        let complete = work.at >= work.dirs.len();
        if ended || complete {
            self.finish_verify(complete, clock);
            return StepResult::Progress;
        }
        let d = &work.dirs[work.at];
        let res = self.slot.watch_io(clock.now(), || sh.backend.stat_dir(&d.loc));
        let i = u32::try_from(work.at).unwrap_or(u32::MAX);
        match res {
            // The call is still in progress as far as the watchdog is concerned.
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => return StepResult::Blocked,
            Ok(m) if m.kind == Kind::Dir && (m.dev, m.ino) == (d.dev, d.ino) => {
                if d.mtime != Some(m.mtime) {
                    work.changed.push(i);
                }
            }
            _ => work.gone.push(i),
        }
        work.at += 1;
        StepResult::Progress
    }

    /// Ends the running batch task with what it found, `complete` when it stat'ed every
    /// directory.
    #[expect(
        clippy::expect_used,
        reason = "called only by the step of a running batch task"
    )]
    fn finish_verify(&mut self, complete: bool, clock: &dyn Clock) {
        let mut r = self.running.take().expect("running");
        let work = r.task.verify.take().expect("batch task");
        self.report_verify(&r.task, *work, complete, clock);
    }

    /// Sends what batch `work` of `task` found and ends its part.
    pub(super) fn report_verify(
        &self,
        task: &Task<B::Loc>,
        work: VerifyWork<B::Loc>,
        complete: bool,
        clock: &dyn Clock,
    ) {
        let mut ob = self.slot.outbox.lock();
        ob.push(
            ScanMsg::Verified {
                job: task.job,
                batch: work.batch,
                changed: work.changed,
                gone: work.gone,
                complete,
            },
            clock.now(),
        );
        self.end_part(&mut ob, task, LISTING_PART);
    }
}
