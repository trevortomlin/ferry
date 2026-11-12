// The transfer queue.
//
// Plain data owned by the UI thread. The engine reports what it has done and
// this folds it in; nothing here does I/O, so the rules about what a transfer
// is allowed to do next are directly testable.
//
// Finished transfers are kept until dismissed, because "did that upload
// actually work?" is a question people ask after the fact.

use std::time::{Duration, Instant};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct TransferId(pub u64);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    Download,
    Upload,
}

impl Direction {
    pub fn label(self) -> &'static str {
        match self {
            Self::Download => "Download",
            Self::Upload => "Upload",
        }
    }
}

/// What is already sitting at the destination.
///
/// `partial` is an interrupted transfer of the same file; `existing` is a
/// complete-looking file. Both can be present: an earlier attempt was
/// interrupted *and* an older copy is there.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Conflict {
    pub existing: Option<u64>,
    pub partial: Option<u64>,
    pub source: u64,
}

impl Conflict {
    /// Whether there is anything to decide about.
    pub fn blocks(&self) -> bool {
        self.existing.is_some() || self.partial.is_some()
    }

    /// Resuming only makes sense when the interrupted transfer is shorter than
    /// the file it came from. A partial that is longer means the source
    /// changed, and continuing would splice two different files together.
    pub fn can_resume(&self) -> bool {
        self.partial.is_some_and(|at| at > 0 && at < self.source)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Resolution {
    /// Start again from zero, discarding whatever is there.
    Overwrite,
    /// Continue an interrupted transfer.
    Resume,
    Skip,
    /// Transfer under a free name, keeping what is already there.
    KeepBoth,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum State {
    /// Accepted, not started. The engine runs one at a time per session.
    Queued,
    /// The destination has been asked about; waiting for the answer.
    Probing,
    /// Something is already there and the user has to choose.
    Blocked(Conflict),
    Running,
    Done,
    /// Stopped on request. Distinct from failure: nothing went wrong.
    Cancelled,
    Failed(String),
}

impl State {
    pub fn is_finished(&self) -> bool {
        matches!(self, Self::Done | Self::Cancelled | Self::Failed(_))
    }

    /// Occupying the connection, so nothing else may start.
    pub fn is_busy(&self) -> bool {
        matches!(self, Self::Probing | Self::Running)
    }
}

#[derive(Debug, Clone)]
pub struct Transfer {
    pub id: TransferId,
    pub direction: Direction,
    /// What to show: the file's name, not its whole path.
    pub name: String,
    pub remote: String,
    pub local: std::path::PathBuf,
    /// `0` when the size is not known yet.
    pub total: u64,
    pub done: u64,
    /// Byte offset an interrupted transfer picks up from. `0` starts fresh.
    pub resume_from: u64,
    pub state: State,
    started: Option<Instant>,
    finished_at: Option<Instant>,
}

impl Transfer {
    /// `0.0` to `1.0`, or `None` when the total is unknown so the bar can be
    /// indeterminate rather than pretending to be at zero.
    pub fn fraction(&self) -> Option<f32> {
        (self.total > 0).then(|| (self.done as f32 / self.total as f32).clamp(0.0, 1.0))
    }

    /// Bytes per second so far, once there is enough to be meaningful.
    pub fn rate(&self) -> Option<f64> {
        let started = self.started?;
        let elapsed = started.elapsed().as_secs_f64();
        (elapsed > 0.25 && self.done > 0).then(|| self.done as f64 / elapsed)
    }

    /// Seconds left at the current rate.
    pub fn remaining(&self) -> Option<Duration> {
        let rate = self.rate()?;
        let left = self.total.checked_sub(self.done)?;
        (rate > 0.0 && self.total > 0).then(|| Duration::from_secs_f64(left as f64 / rate))
    }
}

#[derive(Debug, Default)]
pub struct Queue {
    transfers: Vec<Transfer>,
    next_id: u64,
}

impl Queue {
    pub fn add(
        &mut self,
        direction: Direction,
        remote: String,
        local: std::path::PathBuf,
        total: u64,
    ) -> TransferId {
        self.next_id += 1;
        let id = TransferId(self.next_id);
        let name = match direction {
            Direction::Download => crate::model::path::file_name(&remote).to_owned(),
            Direction::Upload => local
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default(),
        };
        self.transfers.push(Transfer {
            id,
            direction,
            name,
            remote,
            local,
            total,
            done: 0,
            resume_from: 0,
            state: State::Queued,
            started: None,
            finished_at: None,
        });
        id
    }

    pub fn all(&self) -> &[Transfer] {
        &self.transfers
    }

    pub fn is_empty(&self) -> bool {
        self.transfers.is_empty()
    }

    fn find(&mut self, id: TransferId) -> Option<&mut Transfer> {
        self.transfers.iter_mut().find(|t| t.id == id)
    }

    /// The next transfer to run, if nothing is running already. One at a time
    /// per session: SFTP multiplexes over a single connection, and running four
    /// transfers over it makes all four slower rather than any faster.
    pub fn next_queued(&self) -> Option<TransferId> {
        // A blocked transfer does not hold the connection, but it does hold its
        // own place: starting later ones around it would reorder the queue in a
        // way nobody asked for.
        if self.transfers.iter().any(|t| t.state.is_busy()) {
            return None;
        }
        let first_unfinished = self
            .transfers
            .iter()
            .find(|t| !t.state.is_finished() && t.state != State::Queued);
        if first_unfinished.is_some() {
            return None;
        }
        self.transfers
            .iter()
            .find(|t| t.state == State::Queued)
            .map(|t| t.id)
    }

    /// Mark a transfer as waiting to hear what is at its destination.
    pub fn probing(&mut self, id: TransferId) {
        if let Some(transfer) = self.find(id) {
            transfer.state = State::Probing;
        }
    }

    /// The destination answered. Returns true when the user has to decide.
    pub fn blocked(&mut self, id: TransferId, conflict: Conflict) -> bool {
        let Some(transfer) = self.find(id) else {
            return false;
        };
        if conflict.source > 0 {
            transfer.total = conflict.source;
        }
        if conflict.blocks() {
            transfer.state = State::Blocked(conflict);
            true
        } else {
            transfer.state = State::Queued;
            false
        }
    }

    /// Apply a decision to a blocked transfer. Returns the byte offset to start
    /// from, or `None` when the transfer was skipped.
    pub fn resolve(&mut self, id: TransferId, resolution: Resolution) -> Option<u64> {
        let transfer = self.find(id)?;
        let State::Blocked(conflict) = transfer.state.clone() else {
            return None;
        };

        match resolution {
            Resolution::Skip => {
                transfer.state = State::Cancelled;
                transfer.finished_at = Some(Instant::now());
                None
            }
            Resolution::Resume if conflict.can_resume() => {
                let at = conflict.partial.unwrap_or(0);
                transfer.resume_from = at;
                transfer.done = at;
                transfer.state = State::Queued;
                Some(at)
            }
            // Resume was offered but is no longer valid; starting over is the
            // safe reading of "continue".
            Resolution::Resume | Resolution::Overwrite => {
                transfer.resume_from = 0;
                transfer.done = 0;
                transfer.state = State::Queued;
                Some(0)
            }
            Resolution::KeepBoth => {
                transfer.resume_from = 0;
                transfer.done = 0;
                transfer.state = State::Queued;
                let (remote, local) = free_name(&transfer.remote, &transfer.local);
                transfer.remote = remote;
                transfer.local = local;
                transfer.name = crate::model::path::file_name(&transfer.remote).to_owned();
                Some(0)
            }
        }
    }

    /// Everything currently waiting on a decision.
    pub fn blocked_ids(&self) -> Vec<TransferId> {
        self.transfers
            .iter()
            .filter(|t| matches!(t.state, State::Blocked(_)))
            .map(|t| t.id)
            .collect()
    }

    pub fn get(&self, id: TransferId) -> Option<&Transfer> {
        self.transfers.iter().find(|t| t.id == id)
    }

    pub fn start(&mut self, id: TransferId, total: u64) {
        if let Some(transfer) = self.find(id) {
            transfer.state = State::Running;
            transfer.done = transfer.resume_from;
            transfer.started = Some(Instant::now());
            if total > 0 {
                transfer.total = total;
            }
        }
    }

    /// Absolute byte count, not a delta, so a dropped progress message cannot
    /// leave the bar permanently behind.
    pub fn progress(&mut self, id: TransferId, done: u64) {
        if let Some(transfer) = self.find(id)
            && transfer.state == State::Running
        {
            // The engine counts bytes it moved; a resumed transfer is already
            // that far in.
            let done = done + transfer.resume_from;
            transfer.done = done;
            transfer.total = transfer.total.max(done);
        }
    }

    pub fn finish(&mut self, id: TransferId, state: State) {
        if let Some(transfer) = self.find(id) {
            if state == State::Done && transfer.total > 0 {
                // A completed transfer shows a full bar even if the last
                // progress message never arrived.
                transfer.done = transfer.total;
            }
            transfer.state = state;
            transfer.finished_at = Some(Instant::now());
        }
    }

    /// Ask for a transfer to stop. Queued ones stop immediately; running ones
    /// are marked and the engine confirms.
    pub fn request_cancel(&mut self, id: TransferId) -> bool {
        match self.find(id) {
            Some(transfer) if transfer.state == State::Queued => {
                transfer.state = State::Cancelled;
                transfer.finished_at = Some(Instant::now());
                false
            }
            Some(transfer) if transfer.state == State::Running => true,
            _ => false,
        }
    }

    pub fn remove(&mut self, id: TransferId) {
        self.transfers.retain(|t| t.id != id);
    }

    pub fn clear_finished(&mut self) {
        self.transfers.retain(|t| !t.state.is_finished());
    }

    pub fn active(&self) -> usize {
        self.transfers
            .iter()
            .filter(|t| !t.state.is_finished())
            .count()
    }

    pub fn failed(&self) -> usize {
        self.transfers
            .iter()
            .filter(|t| matches!(t.state, State::Failed(_)))
            .count()
    }

    /// Combined progress across everything unfinished, for the status bar.
    pub fn overall(&self) -> Option<(u64, u64)> {
        let mut done = 0;
        let mut total = 0;
        let mut any = false;
        for transfer in self.transfers.iter().filter(|t| !t.state.is_finished()) {
            any = true;
            done += transfer.done;
            total += transfer.total;
        }
        (any && total > 0).then_some((done, total))
    }

    /// Drop successful transfers that finished a while ago, so the panel does
    /// not grow without bound during a long session. Failures stay: they need
    /// to be seen.
    pub fn expire(&mut self, after: Duration) {
        self.transfers.retain(|transfer| {
            if transfer.state != State::Done {
                return true;
            }
            transfer.finished_at.is_none_or(|at| at.elapsed() < after)
        });
    }
}

/// A destination name that is not taken, by inserting a counter before the
/// extension the way every file manager does: `report.pdf` becomes
/// `report (2).pdf`.
fn free_name(remote: &str, local: &std::path::Path) -> (String, std::path::PathBuf) {
    let name = crate::model::path::file_name(remote);
    let (stem, extension) = match name.rsplit_once('.') {
        // A leading dot is part of the name, not an extension.
        Some((stem, extension)) if !stem.is_empty() => (stem, format!(".{extension}")),
        _ => (name, String::new()),
    };

    let mut counter = 2;
    loop {
        let candidate = format!("{stem} ({counter}){extension}");
        let local_candidate = local.with_file_name(&candidate);
        if !local_candidate.exists() || counter > 999 {
            let parent = crate::model::path::parent(remote).unwrap_or("/");
            return (crate::model::path::join(parent, &candidate), local_candidate);
        }
        counter += 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn queue_with_one() -> (Queue, TransferId) {
        let mut queue = Queue::default();
        let id = queue.add(
            Direction::Download,
            "/remote/report.pdf".to_owned(),
            PathBuf::from("/tmp/report.pdf"),
            0,
        );
        (queue, id)
    }

    #[test]
    fn a_new_transfer_is_queued_and_named_after_its_file() {
        let (queue, id) = queue_with_one();
        let transfer = &queue.all()[0];
        assert_eq!(transfer.id, id);
        assert_eq!(transfer.name, "report.pdf");
        assert_eq!(transfer.state, State::Queued);
        assert_eq!(queue.active(), 1);
    }

    #[test]
    fn an_upload_is_named_after_the_local_file() {
        let mut queue = Queue::default();
        queue.add(
            Direction::Upload,
            "/remote/dir/whatever".to_owned(),
            PathBuf::from("/home/ferry/photo.png"),
            10,
        );
        assert_eq!(queue.all()[0].name, "photo.png");
    }

    #[test]
    fn only_one_transfer_runs_at_a_time() {
        let mut queue = Queue::default();
        let first = queue.add(Direction::Download, "/a".into(), "/tmp/a".into(), 10);
        let second = queue.add(Direction::Download, "/b".into(), "/tmp/b".into(), 10);

        assert_eq!(queue.next_queued(), Some(first));
        queue.start(first, 10);
        assert_eq!(queue.next_queued(), None, "one at a time");

        queue.finish(first, State::Done);
        assert_eq!(queue.next_queued(), Some(second));
    }

    #[test]
    fn progress_is_absolute_so_a_lost_message_cannot_strand_the_bar() {
        let (mut queue, id) = queue_with_one();
        queue.start(id, 1000);
        queue.progress(id, 250);
        assert_eq!(queue.all()[0].fraction(), Some(0.25));
        // A later message with a larger count simply wins; nothing accumulates.
        queue.progress(id, 900);
        assert_eq!(queue.all()[0].fraction(), Some(0.9));
    }

    #[test]
    fn progress_is_ignored_once_a_transfer_has_finished() {
        let (mut queue, id) = queue_with_one();
        queue.start(id, 1000);
        queue.finish(id, State::Cancelled);
        queue.progress(id, 999);
        assert_eq!(queue.all()[0].done, 0);
    }

    #[test]
    fn completing_fills_the_bar_even_without_a_final_progress_message() {
        let (mut queue, id) = queue_with_one();
        queue.start(id, 1000);
        queue.progress(id, 400);
        queue.finish(id, State::Done);
        assert_eq!(queue.all()[0].fraction(), Some(1.0));
    }

    #[test]
    fn an_unknown_size_has_no_fraction_rather_than_zero() {
        let (mut queue, id) = queue_with_one();
        queue.start(id, 0);
        assert_eq!(queue.all()[0].fraction(), None);
        // Once bytes arrive, the total grows to at least what has been seen.
        queue.progress(id, 500);
        assert_eq!(queue.all()[0].fraction(), Some(1.0));
    }

    #[test]
    fn cancelling_a_queued_transfer_needs_no_engine_round_trip() {
        let (mut queue, id) = queue_with_one();
        assert!(
            !queue.request_cancel(id),
            "a queued transfer stops without asking the engine"
        );
        assert_eq!(queue.all()[0].state, State::Cancelled);

        let (mut queue, id) = queue_with_one();
        queue.start(id, 10);
        assert!(
            queue.request_cancel(id),
            "a running transfer must be stopped by the engine"
        );
        assert_eq!(queue.all()[0].state, State::Running, "not yet confirmed");
    }

    #[test]
    fn failures_are_counted_and_kept() {
        let (mut queue, id) = queue_with_one();
        queue.start(id, 10);
        queue.finish(id, State::Failed("Permission denied".to_owned()));
        assert_eq!(queue.failed(), 1);
        assert_eq!(queue.active(), 0);

        queue.clear_finished();
        assert!(queue.is_empty());
    }

    #[test]
    fn successes_expire_but_failures_do_not() {
        let mut queue = Queue::default();
        let good = queue.add(Direction::Download, "/a".into(), "/tmp/a".into(), 1);
        let bad = queue.add(Direction::Download, "/b".into(), "/tmp/b".into(), 1);
        queue.finish(good, State::Done);
        queue.finish(bad, State::Failed("nope".to_owned()));

        queue.expire(Duration::from_secs(0));
        let remaining: Vec<_> = queue.all().iter().map(|t| t.id).collect();
        assert_eq!(remaining, [bad], "a failure must not disappear on its own");
    }

    #[test]
    fn overall_progress_covers_only_unfinished_work() {
        let mut queue = Queue::default();
        let first = queue.add(Direction::Download, "/a".into(), "/tmp/a".into(), 100);
        let second = queue.add(Direction::Download, "/b".into(), "/tmp/b".into(), 300);
        queue.start(first, 100);
        queue.progress(first, 50);

        assert_eq!(queue.overall(), Some((50, 400)));

        queue.finish(first, State::Done);
        assert_eq!(queue.overall(), Some((0, 300)), "done work drops out");

        queue.finish(second, State::Done);
        assert_eq!(queue.overall(), None);
    }

    fn conflict(existing: Option<u64>, partial: Option<u64>, source: u64) -> Conflict {
        Conflict {
            existing,
            partial,
            source,
        }
    }

    #[test]
    fn a_clear_destination_goes_straight_to_queued() {
        let (mut queue, id) = queue_with_one();
        queue.probing(id);
        assert!(!queue.blocked(id, conflict(None, None, 500)));
        assert_eq!(queue.all()[0].state, State::Queued);
        // The probe also told us how big the source is.
        assert_eq!(queue.all()[0].total, 500);
    }

    #[test]
    fn an_existing_file_blocks_until_answered() {
        let (mut queue, id) = queue_with_one();
        queue.probing(id);
        assert!(queue.blocked(id, conflict(Some(100), None, 500)));
        assert_eq!(queue.blocked_ids(), [id]);
        assert_eq!(
            queue.next_queued(),
            None,
            "a blocked transfer holds its place"
        );
    }

    #[test]
    fn resume_is_offered_only_for_a_shorter_partial() {
        assert!(conflict(None, Some(100), 500).can_resume());
        // Nothing transferred yet.
        assert!(!conflict(None, Some(0), 500).can_resume());
        // Same size: there is nothing left to do, so this is a plain overwrite
        // question.
        assert!(!conflict(None, Some(500), 500).can_resume());
        // Longer than the source means the source changed underneath.
        assert!(!conflict(None, Some(900), 500).can_resume());
        assert!(!conflict(Some(500), None, 500).can_resume());
    }

    #[test]
    fn resuming_starts_from_the_partial_and_counts_it_as_done() {
        let (mut queue, id) = queue_with_one();
        queue.probing(id);
        queue.blocked(id, conflict(None, Some(120), 500));

        assert_eq!(queue.resolve(id, Resolution::Resume), Some(120));
        let transfer = &queue.all()[0];
        assert_eq!(transfer.resume_from, 120);
        assert_eq!(transfer.done, 120);
        assert_eq!(transfer.state, State::Queued);

        // Progress from the engine counts only the bytes it moved.
        queue.start(id, 500);
        queue.progress(id, 80);
        assert_eq!(queue.all()[0].done, 200);
        assert_eq!(queue.all()[0].fraction(), Some(0.4));
    }

    #[test]
    fn asking_to_resume_something_unresumable_starts_over_instead() {
        let (mut queue, id) = queue_with_one();
        queue.probing(id);
        // Partial longer than the source: continuing would splice two files.
        queue.blocked(id, conflict(None, Some(900), 500));
        assert_eq!(queue.resolve(id, Resolution::Resume), Some(0));
        assert_eq!(queue.all()[0].resume_from, 0);
    }

    #[test]
    fn overwriting_discards_any_partial() {
        let (mut queue, id) = queue_with_one();
        queue.probing(id);
        queue.blocked(id, conflict(Some(500), Some(120), 500));
        assert_eq!(queue.resolve(id, Resolution::Overwrite), Some(0));
        assert_eq!(queue.all()[0].resume_from, 0);
        assert_eq!(queue.all()[0].done, 0);
    }

    #[test]
    fn skipping_finishes_the_transfer_without_running_it() {
        let (mut queue, id) = queue_with_one();
        queue.probing(id);
        queue.blocked(id, conflict(Some(500), None, 500));
        assert_eq!(queue.resolve(id, Resolution::Skip), None);
        assert_eq!(queue.all()[0].state, State::Cancelled);
        assert_eq!(queue.active(), 0);
    }

    #[test]
    fn keeping_both_picks_a_free_name_around_the_extension() {
        let mut queue = Queue::default();
        let id = queue.add(
            Direction::Download,
            "/remote/report.pdf".to_owned(),
            std::env::temp_dir().join("ferry-conflict-test.pdf"),
            0,
        );
        queue.probing(id);
        queue.blocked(id, conflict(Some(10), None, 500));
        assert_eq!(queue.resolve(id, Resolution::KeepBoth), Some(0));

        let transfer = &queue.all()[0];
        assert_eq!(transfer.remote, "/remote/report (2).pdf");
        assert_eq!(transfer.name, "report (2).pdf");
        assert!(
            transfer.local.to_string_lossy().ends_with("(2).pdf"),
            "{:?}",
            transfer.local
        );
    }

    #[test]
    fn a_dotfile_keeps_its_leading_dot_when_renamed() {
        assert_eq!(
            free_name("/a/.bashrc", std::path::Path::new("/tmp/.bashrc")).0,
            "/a/.bashrc (2)"
        );
        assert_eq!(
            free_name("/a/archive.tar.gz", std::path::Path::new("/tmp/x.tar.gz")).0,
            "/a/archive.tar (2).gz"
        );
        assert_eq!(
            free_name("/a/noext", std::path::Path::new("/tmp/noext")).0,
            "/a/noext (2)"
        );
    }

    #[test]
    fn a_rate_appears_only_once_there_is_something_to_measure() {
        let (mut queue, id) = queue_with_one();
        queue.start(id, 1000);
        // No elapsed time and no bytes yet.
        assert_eq!(queue.all()[0].rate(), None);
        assert_eq!(queue.all()[0].remaining(), None);
    }
}
