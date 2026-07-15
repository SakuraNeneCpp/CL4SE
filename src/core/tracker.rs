//! OS-independent heuristic composition tracker.

use std::time::Duration;

use super::ImeGuess;

/// The tracker's conservative estimate of the current composition state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompositionState {
    Idle,
    Composing,
}

/// Tracks whether observed input is likely to belong to an active IME composition.
///
/// Time is supplied by the caller as a monotonic duration. This keeps the state
/// machine deterministic and avoids calling [`std::time::Instant`] in core logic.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompositionTracker {
    state: CompositionState,
    last_key_at: Option<Duration>,
    timeout: Duration,
}

impl CompositionTracker {
    pub fn new(timeout: Duration) -> Self {
        Self {
            state: CompositionState::Idle,
            last_key_at: None,
            timeout,
        }
    }

    pub const fn state(&self) -> CompositionState {
        self.state
    }

    pub const fn is_composing(&self) -> bool {
        matches!(self.state, CompositionState::Composing)
    }

    /// Applies a newly observed IME state and the timeout rule.
    pub fn refresh(&mut self, ime_active: ImeGuess, now: Duration) {
        if ime_active != ImeGuess::Yes {
            self.reset();
            return;
        }

        if self.is_composing() {
            let within_timeout = self
                .last_key_at
                .and_then(|last_key_at| now.checked_sub(last_key_at))
                .is_some_and(|elapsed| elapsed < self.timeout);

            if !within_timeout {
                self.reset();
            }
        }
    }

    /// Records a non-repeated, physical printable key press.
    pub fn printable_key_down(&mut self, ime_active: ImeGuess, now: Duration) {
        self.refresh(ime_active, now);
        if ime_active == ImeGuess::Yes {
            self.set_state(CompositionState::Composing);
            self.last_key_at = Some(now);
        }
    }

    /// Records Enter, Escape, or Ctrl+M, whether physical or self-injected.
    pub fn commit_like_key_down(&mut self) {
        self.reset();
    }

    pub fn mouse_click(&mut self) {
        self.reset();
    }

    pub fn focus_changed(&mut self) {
        self.reset();
    }

    /// Clears all heuristic state. Unknown inputs always use this safe path.
    pub fn reset(&mut self) {
        self.set_state(CompositionState::Idle);
        self.last_key_at = None;
    }

    fn set_state(&mut self, state: CompositionState) {
        if self.state != state {
            log::debug!("composition state: {:?} -> {:?}", self.state, state);
            self.state = state;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const TIMEOUT: Duration = Duration::from_secs(30);

    fn composing_tracker() -> CompositionTracker {
        let mut tracker = CompositionTracker::new(TIMEOUT);
        tracker.printable_key_down(ImeGuess::Yes, Duration::ZERO);
        tracker
    }

    #[test]
    fn idle_printable_with_active_ime_starts_composition() {
        let tracker = composing_tracker();

        assert_eq!(tracker.state(), CompositionState::Composing);
    }

    #[test]
    fn idle_printable_without_known_active_ime_stays_idle() {
        for ime_active in [ImeGuess::No, ImeGuess::Unknown] {
            let mut tracker = CompositionTracker::new(TIMEOUT);
            tracker.printable_key_down(ime_active, Duration::ZERO);
            assert_eq!(tracker.state(), CompositionState::Idle);
        }
    }

    #[test]
    fn another_printable_key_keeps_composing_and_refreshes_timeout() {
        let mut tracker = composing_tracker();
        tracker.printable_key_down(ImeGuess::Yes, Duration::from_secs(20));
        tracker.refresh(ImeGuess::Yes, Duration::from_secs(49));

        assert!(tracker.is_composing());
    }

    #[test]
    fn commit_like_key_resets_composition() {
        let mut tracker = composing_tracker();
        tracker.commit_like_key_down();

        assert_eq!(tracker.state(), CompositionState::Idle);
    }

    #[test]
    fn mouse_click_resets_composition() {
        let mut tracker = composing_tracker();
        tracker.mouse_click();

        assert_eq!(tracker.state(), CompositionState::Idle);
    }

    #[test]
    fn focus_change_resets_composition() {
        let mut tracker = composing_tracker();
        tracker.focus_changed();

        assert_eq!(tracker.state(), CompositionState::Idle);
    }

    #[test]
    fn ime_no_or_unknown_resets_composition() {
        for ime_active in [ImeGuess::No, ImeGuess::Unknown] {
            let mut tracker = composing_tracker();
            tracker.refresh(ime_active, Duration::from_secs(1));
            assert_eq!(tracker.state(), CompositionState::Idle);
        }
    }

    #[test]
    fn timeout_expires_at_the_exact_boundary() {
        let mut tracker = composing_tracker();
        tracker.refresh(ImeGuess::Yes, TIMEOUT - Duration::from_nanos(1));
        assert!(tracker.is_composing());

        tracker.refresh(ImeGuess::Yes, TIMEOUT);
        assert_eq!(tracker.state(), CompositionState::Idle);
    }

    #[test]
    fn backwards_time_resets_instead_of_guessing() {
        let mut tracker = CompositionTracker::new(TIMEOUT);
        tracker.printable_key_down(ImeGuess::Yes, Duration::from_secs(10));
        tracker.refresh(ImeGuess::Yes, Duration::from_secs(9));

        assert_eq!(tracker.state(), CompositionState::Idle);
    }
}
