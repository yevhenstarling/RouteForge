//! Run control shared by every stage of a search.

use std::fmt;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

use crate::tour::Tour;

/// How many iterations pass between two looks at the cancel flag and the clock.
pub(crate) const POLL_INTERVAL: u64 = 1 << 14;

/// What stopped a search.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StopReason {
    /// The iteration budget is used up.
    Budget,
    /// The deadline has passed.
    Deadline,
    /// The cancel flag was set.
    Cancelled,
}

/// How far a running search has got. The progress callback is handed this together
/// with the best tour so far.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Progress {
    /// Candidate moves evaluated so far.
    pub iterations: u64,
    /// The length of the best tour so far, in metres.
    pub best_cost: u64,
}

/// What a search calls to say how far it has got, and with which tour.
type Report<'a> = Box<dyn FnMut(Progress, &Tour) + 'a>;

/// The limits of one search and its bookkeeping, shared by all stages (D15).
///
/// One iteration is one candidate move evaluated, in local search and in annealing
/// alike. A control always carries a limit, an iteration budget or a deadline, because
/// annealing never ends on its own.
///
/// The budget, the deadline and the cancel flag only ever stop a search. They never
/// steer it, so a search that is stopped early is the beginning of the longer one (D5).
/// Once stopped, a control stays stopped: a stage called with it does nothing.
pub struct Control<'a> {
    max_iterations: u64,
    deadline: Option<Instant>,
    cancel: Option<&'a AtomicBool>,
    report: Option<Report<'a>>,
    iterations: u64,
    next_poll: u64,
    stop_reason: Option<StopReason>,
}

impl<'a> Control<'a> {
    /// A search that evaluates at most `max` candidate moves.
    pub fn max_iterations(max: u64) -> Self {
        Self {
            max_iterations: max,
            deadline: None,
            cancel: None,
            report: None,
            iterations: 0,
            next_poll: 0,
            stop_reason: None,
        }
    }

    /// A search that runs until `deadline`.
    pub fn until(deadline: Instant) -> Self {
        Self::max_iterations(u64::MAX).with_deadline(deadline)
    }

    /// Also stops the search after `max` candidate moves.
    pub fn with_max_iterations(mut self, max: u64) -> Self {
        self.max_iterations = max;
        self
    }

    /// Also stops the search at `deadline`.
    pub fn with_deadline(mut self, deadline: Instant) -> Self {
        self.deadline = Some(deadline);
        self
    }

    /// Stops the search once `flag` is `true`.
    ///
    /// The flag and the deadline are looked at every 16 384 iterations, which is well
    /// under a millisecond of work.
    pub fn with_cancel(mut self, flag: &'a AtomicBool) -> Self {
        self.cancel = Some(flag);
        self
    }

    /// Calls `report` every 16 384 iterations while the search runs, with how far the
    /// search has got and the best tour it has found so far.
    ///
    /// The callback looks on and nothing more: the search takes the same course with it
    /// as without it (D5).
    pub fn with_progress(mut self, report: impl FnMut(Progress, &Tour) + 'a) -> Self {
        self.report = Some(Box::new(report));
        self
    }

    /// Candidate moves evaluated so far.
    pub fn iterations(&self) -> u64 {
        self.iterations
    }

    /// What stopped the search, or `None` if nothing has.
    pub fn stop_reason(&self) -> Option<StopReason> {
        self.stop_reason
    }

    /// Asks for `moves` more iterations. `best` is the best tour so far, for the
    /// progress callback.
    ///
    /// Returns `false` when the search has to stop. Nothing is charged then, and the
    /// caller must not evaluate the moves.
    #[inline]
    pub(crate) fn spend(&mut self, moves: u64, best: &Tour) -> bool {
        if self.stop_reason.is_some() {
            return false;
        }
        if self.iterations >= self.next_poll && !self.poll(best) {
            return false;
        }
        if moves > self.max_iterations.saturating_sub(self.iterations) {
            self.stop_reason = Some(StopReason::Budget);
            return false;
        }
        self.iterations += moves;
        true
    }

    /// The slow part of [`spend`](Self::spend): the cancel flag, the clock and the callback.
    #[cold]
    fn poll(&mut self, best: &Tour) -> bool {
        self.next_poll = self.iterations.saturating_add(POLL_INTERVAL);
        if self.cancel.is_some_and(|flag| flag.load(Ordering::Relaxed)) {
            self.stop_reason = Some(StopReason::Cancelled);
        } else if self
            .deadline
            .is_some_and(|deadline| Instant::now() >= deadline)
        {
            self.stop_reason = Some(StopReason::Deadline);
        } else {
            self.report(best);
        }
        self.stop_reason.is_none()
    }

    /// Hands the callback the best tour so far, which the tour is throughout: local
    /// search only ever shortens the tour it is given, and annealing keeps its best
    /// tour apart from the one it moves about.
    pub(crate) fn report(&mut self, best: &Tour) {
        if let Some(report) = self.report.as_mut() {
            let progress = Progress {
                iterations: self.iterations,
                best_cost: best.cost,
            };
            report(progress, best);
        }
    }
}

impl fmt::Debug for Control<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Control")
            .field("max_iterations", &self.max_iterations)
            .field("deadline", &self.deadline)
            .field("iterations", &self.iterations)
            .field("stop_reason", &self.stop_reason)
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    /// A tour of the depot alone that claims to be `cost` metres long. A control only
    /// passes its tour on, so what the tour looks like does not matter here.
    fn tour(cost: u64) -> Tour {
        Tour {
            order: vec![0],
            cost,
        }
    }

    #[test]
    fn the_budget_is_never_exceeded() {
        let mut control = Control::max_iterations(10);
        assert!(control.spend(4, &tour(0)));
        assert!(control.spend(6, &tour(0)));
        assert!(!control.spend(1, &tour(0)));
        assert_eq!(control.iterations(), 10);
        assert_eq!(control.stop_reason(), Some(StopReason::Budget));
    }

    #[test]
    fn a_step_the_budget_cannot_cover_is_not_charged() {
        let mut control = Control::max_iterations(10);
        assert!(control.spend(7, &tour(0)));
        assert!(!control.spend(4, &tour(0)));
        assert_eq!(control.iterations(), 7);
    }

    #[test]
    fn a_stop_is_final() {
        let mut control = Control::max_iterations(1);
        assert!(!control.spend(2, &tour(0)));
        assert!(!control.spend(1, &tour(0)));
        assert_eq!(control.iterations(), 0);
    }

    #[test]
    fn a_flag_set_beforehand_stops_the_first_step() {
        let flag = AtomicBool::new(true);
        let mut control = Control::max_iterations(u64::MAX).with_cancel(&flag);
        assert!(!control.spend(1, &tour(0)));
        assert_eq!(control.iterations(), 0);
        assert_eq!(control.stop_reason(), Some(StopReason::Cancelled));
    }

    #[test]
    fn a_flag_is_noticed_within_one_poll_interval() {
        let flag = AtomicBool::new(false);
        let mut control = Control::max_iterations(u64::MAX).with_cancel(&flag);
        assert!(control.spend(1, &tour(0)));
        flag.store(true, Ordering::Relaxed);
        while control.spend(1, &tour(0)) {}
        assert_eq!(control.iterations(), POLL_INTERVAL);
        assert_eq!(control.stop_reason(), Some(StopReason::Cancelled));
    }

    #[test]
    fn a_deadline_in_the_past_stops_the_first_step() {
        let mut control = Control::until(Instant::now());
        assert!(!control.spend(1, &tour(0)));
        assert_eq!(control.iterations(), 0);
        assert_eq!(control.stop_reason(), Some(StopReason::Deadline));
    }

    #[test]
    fn a_deadline_ahead_lets_the_search_run() {
        let mut control = Control::until(Instant::now() + Duration::from_secs(3600));
        assert!(control.spend(1, &tour(0)));
        assert_eq!(control.stop_reason(), None);
    }

    #[test]
    fn progress_is_reported_once_per_poll_interval() {
        let mut reports = Vec::new();
        let mut control = Control::max_iterations(3 * POLL_INTERVAL)
            .with_progress(|progress, best| reports.push((progress, best.clone())));
        let mut best = tour(1_000);
        while control.spend(1, &best) {
            best.cost -= u64::from(control.iterations() == POLL_INTERVAL);
        }
        drop(control);
        // Each report comes with the tour that was the best at that moment.
        let report = |iterations, best_cost| {
            let progress = Progress {
                iterations,
                best_cost,
            };
            (progress, tour(best_cost))
        };
        assert_eq!(
            reports,
            [
                report(0, 1_000),
                report(POLL_INTERVAL, 999),
                report(2 * POLL_INTERVAL, 999),
                report(3 * POLL_INTERVAL, 999),
            ]
        );
    }
}
