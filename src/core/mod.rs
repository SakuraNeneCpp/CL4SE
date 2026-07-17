//! OS-independent decision logic.

use std::time::Duration;

use crate::{
    config::{CommitKeyConfig, Config, IdleAction},
    platform::ImeStateProvider,
};

pub mod tracker;

use tracker::CompositionTracker;

const WINDOWS_CTRL_M_IME_IDS: &[&str] = &["ms-ime", "google-japanese-input"];
const LINUX_CTRL_M_IME_IDS: &[&str] = &["mozc", "mozc-jp", "fcitx5-mozc", "ibus-mozc"];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Platform {
    Windows,
    MacOs,
    Linux,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImeGuess {
    Yes,
    No,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommitKey {
    Enter,
    CtrlM,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImeSnapshot {
    pub active: ImeGuess,
    pub ime_id: Option<String>,
}

/// An event already classified by the platform interceptor.
///
/// `self_injected` must only be set when CL4SE's private injection marker
/// matches. Generic events injected by other software are not trusted as ours.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ObservedEvent {
    TriggerKeyDown {
        shift: bool,
        other_mods: bool,
        repeat: bool,
        self_injected: bool,
    },
    PrintableKeyDown {
        repeat: bool,
        self_injected: bool,
    },
    CommitLikeKeyDown {
        repeat: bool,
        self_injected: bool,
    },
    MouseClick,
    FocusChanged,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decision {
    InjectCommitKey(CommitKey),
    InjectShiftEnter,
    PassThroughCapsLock,
    Suppress,
    Ignore,
}

#[derive(Debug, Clone)]
pub struct Engine {
    tracker: CompositionTracker,
    idle_action: IdleAction,
    shift_passthrough: bool,
    commit_key: CommitKeyConfig,
    platform: Platform,
}

impl Engine {
    pub fn new(config: &Config, platform: Platform) -> Self {
        Self {
            tracker: CompositionTracker::new(Duration::from_secs(
                config.detection.heuristic_timeout_secs,
            )),
            idle_action: config.general.idle_action,
            shift_passthrough: config.general.shift_passthrough,
            commit_key: config.general.commit_key,
            platform,
        }
    }

    pub const fn is_composing(&self) -> bool {
        self.tracker.is_composing()
    }

    /// Clears heuristic state after the platform reports dropped observations.
    pub fn reset_composition(&mut self) {
        self.tracker.reset();
    }

    /// Processes one classified event at a caller-supplied monotonic timestamp.
    pub fn handle_event(
        &mut self,
        event: ObservedEvent,
        ime_state: &mut dyn ImeStateProvider,
        now: Duration,
    ) -> Decision {
        if event.is_self_injected() {
            self.tracker.reset();
            log::debug!("self-injected marked event ignored; composition state reset");
            return Decision::Ignore;
        }

        match event {
            ObservedEvent::TriggerKeyDown {
                shift,
                other_mods,
                repeat,
                self_injected: false,
            } => {
                // A physical Caps Lock must remain suppressed even when its
                // logical action is ignored, unless pass-through is explicit.
                if repeat {
                    log::debug!("trigger suppressed: key repeat");
                    return Self::log_trigger_decision(Decision::Suppress);
                }
                if other_mods {
                    log::debug!("trigger suppressed: Ctrl/Alt/Win/Cmd modifier active");
                    return Self::log_trigger_decision(Decision::Suppress);
                }
                if shift && self.shift_passthrough {
                    return Self::log_trigger_decision(Decision::PassThroughCapsLock);
                }

                let snapshot = ime_state.snapshot();
                self.tracker.refresh(snapshot.active, now);
                let decision = if snapshot.active == ImeGuess::Yes && self.tracker.is_composing() {
                    Decision::InjectCommitKey(resolve_commit_key(
                        self.commit_key,
                        snapshot.ime_id.as_deref(),
                        self.platform,
                    ))
                } else if snapshot.active != ImeGuess::Unknown && !self.tracker.is_composing() {
                    self.idle_decision()
                } else {
                    // Unknown must never authorize an injected key, including
                    // the opt-in Shift+Enter idle action.
                    Decision::Suppress
                };
                Self::log_trigger_decision(decision)
            }
            ObservedEvent::PrintableKeyDown {
                repeat,
                self_injected: false,
            } => {
                if !repeat {
                    let snapshot = ime_state.snapshot();
                    self.tracker.printable_key_down(snapshot.active, now);
                }
                Decision::Ignore
            }
            ObservedEvent::CommitLikeKeyDown {
                repeat,
                self_injected: false,
            } => {
                if !repeat {
                    self.tracker.commit_like_key_down();
                }
                Decision::Ignore
            }
            ObservedEvent::MouseClick => {
                self.tracker.mouse_click();
                Decision::Ignore
            }
            ObservedEvent::FocusChanged => {
                self.tracker.focus_changed();
                Decision::Ignore
            }
            // `is_self_injected` returned above, so these arms are unreachable.
            ObservedEvent::TriggerKeyDown {
                self_injected: true,
                ..
            }
            | ObservedEvent::PrintableKeyDown {
                self_injected: true,
                ..
            }
            | ObservedEvent::CommitLikeKeyDown {
                self_injected: true,
                ..
            } => Decision::Ignore,
        }
    }

    fn idle_decision(&self) -> Decision {
        match self.idle_action {
            IdleAction::None => Decision::Suppress,
            IdleAction::ShiftEnter => Decision::InjectShiftEnter,
            IdleAction::CapsLock => Decision::PassThroughCapsLock,
        }
    }

    fn log_trigger_decision(decision: Decision) -> Decision {
        log::debug!("trigger decision: {decision:?}");
        decision
    }
}

impl ObservedEvent {
    const fn is_self_injected(self) -> bool {
        match self {
            Self::TriggerKeyDown { self_injected, .. }
            | Self::PrintableKeyDown { self_injected, .. }
            | Self::CommitLikeKeyDown { self_injected, .. } => self_injected,
            Self::MouseClick | Self::FocusChanged => false,
        }
    }
}

/// Resolves the configured commit key for an explicitly supplied platform.
pub fn resolve_commit_key(
    config: CommitKeyConfig,
    ime_id: Option<&str>,
    platform: Platform,
) -> CommitKey {
    match config {
        CommitKeyConfig::Enter => CommitKey::Enter,
        CommitKeyConfig::CtrlM => CommitKey::CtrlM,
        CommitKeyConfig::Auto => {
            let ctrl_m_allowed = match platform {
                Platform::Windows => ime_id.is_some_and(|ime_id| {
                    WINDOWS_CTRL_M_IME_IDS
                        .iter()
                        .any(|allowed| ime_id.eq_ignore_ascii_case(allowed))
                }),
                Platform::Linux => ime_id.is_some_and(|ime_id| {
                    LINUX_CTRL_M_IME_IDS
                        .iter()
                        .any(|allowed| ime_id.eq_ignore_ascii_case(allowed))
                }),
                Platform::MacOs => false,
            };

            if ctrl_m_allowed {
                CommitKey::CtrlM
            } else {
                CommitKey::Enter
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        config::{GeneralConfig, LogLevel},
        platform::ImeStateProvider,
    };

    #[derive(Debug)]
    struct MockImeStateProvider {
        snapshot: ImeSnapshot,
        calls: usize,
    }

    impl MockImeStateProvider {
        fn new(active: ImeGuess, ime_id: Option<&str>) -> Self {
            Self {
                snapshot: ImeSnapshot {
                    active,
                    ime_id: ime_id.map(str::to_owned),
                },
                calls: 0,
            }
        }
    }

    impl ImeStateProvider for MockImeStateProvider {
        fn snapshot(&mut self) -> ImeSnapshot {
            self.calls += 1;
            self.snapshot.clone()
        }
    }

    fn config(idle_action: IdleAction, shift_passthrough: bool) -> Config {
        Config {
            general: GeneralConfig {
                idle_action,
                shift_passthrough,
                commit_key: CommitKeyConfig::Enter,
                log_level: LogLevel::Info,
            },
            ..Config::default()
        }
    }

    fn printable() -> ObservedEvent {
        ObservedEvent::PrintableKeyDown {
            repeat: false,
            self_injected: false,
        }
    }

    fn trigger() -> ObservedEvent {
        ObservedEvent::TriggerKeyDown {
            shift: false,
            other_mods: false,
            repeat: false,
            self_injected: false,
        }
    }

    fn start_composing(engine: &mut Engine, provider: &mut MockImeStateProvider) {
        assert_eq!(
            engine.handle_event(printable(), provider, Duration::ZERO),
            Decision::Ignore
        );
        assert!(engine.is_composing());
    }

    #[test]
    fn decision_table_yes_and_composing_injects_commit() {
        let mut engine = Engine::new(&config(IdleAction::None, true), Platform::Linux);
        let mut provider = MockImeStateProvider::new(ImeGuess::Yes, Some("unlisted-ime"));
        start_composing(&mut engine, &mut provider);

        assert_eq!(
            engine.handle_event(trigger(), &mut provider, Duration::from_secs(1)),
            Decision::InjectCommitKey(CommitKey::Enter)
        );
    }

    #[test]
    fn decision_table_yes_and_idle_uses_idle_action() {
        let mut engine = Engine::new(&config(IdleAction::CapsLock, true), Platform::Linux);
        let mut provider = MockImeStateProvider::new(ImeGuess::Yes, None);

        assert_eq!(
            engine.handle_event(trigger(), &mut provider, Duration::ZERO),
            Decision::PassThroughCapsLock
        );
    }

    #[test]
    fn shift_enter_idle_action_requires_known_idle_state() {
        for active in [ImeGuess::Yes, ImeGuess::No] {
            let mut engine = Engine::new(&config(IdleAction::ShiftEnter, true), Platform::Linux);
            let mut provider = MockImeStateProvider::new(active, None);

            assert_eq!(
                engine.handle_event(trigger(), &mut provider, Duration::ZERO),
                Decision::InjectShiftEnter
            );
            assert!(!engine.is_composing());
        }
    }

    #[test]
    fn composing_state_commits_even_when_shift_enter_idle_action_is_enabled() {
        let mut engine = Engine::new(&config(IdleAction::ShiftEnter, true), Platform::Linux);
        let mut provider = MockImeStateProvider::new(ImeGuess::Yes, None);
        start_composing(&mut engine, &mut provider);

        assert_eq!(
            engine.handle_event(trigger(), &mut provider, Duration::from_secs(1)),
            Decision::InjectCommitKey(CommitKey::Enter)
        );
    }

    #[test]
    fn decision_table_no_uses_idle_action_even_if_tracker_was_composing() {
        let mut engine = Engine::new(&config(IdleAction::ShiftEnter, true), Platform::Linux);
        let mut provider = MockImeStateProvider::new(ImeGuess::Yes, None);
        start_composing(&mut engine, &mut provider);
        provider.snapshot.active = ImeGuess::No;

        assert_eq!(
            engine.handle_event(trigger(), &mut provider, Duration::from_secs(1)),
            Decision::InjectShiftEnter
        );
        assert!(!engine.is_composing());
    }

    #[test]
    fn decision_table_no_while_idle_uses_idle_action() {
        let mut engine = Engine::new(&config(IdleAction::CapsLock, true), Platform::Linux);
        let mut provider = MockImeStateProvider::new(ImeGuess::No, None);

        assert_eq!(
            engine.handle_event(trigger(), &mut provider, Duration::ZERO),
            Decision::PassThroughCapsLock
        );
        assert!(!engine.is_composing());
    }

    #[test]
    fn unknown_is_never_treated_as_yes() {
        let mut engine = Engine::new(&config(IdleAction::None, true), Platform::Linux);
        let mut provider = MockImeStateProvider::new(ImeGuess::Yes, None);
        start_composing(&mut engine, &mut provider);
        provider.snapshot.active = ImeGuess::Unknown;

        assert_eq!(
            engine.handle_event(trigger(), &mut provider, Duration::from_secs(1)),
            Decision::Suppress
        );
        assert!(!engine.is_composing());
    }

    #[test]
    fn unknown_never_runs_an_injecting_idle_action() {
        for idle_action in [IdleAction::ShiftEnter, IdleAction::CapsLock] {
            let mut engine = Engine::new(&config(idle_action, true), Platform::Linux);
            let mut provider = MockImeStateProvider::new(ImeGuess::Unknown, None);

            assert_eq!(
                engine.handle_event(trigger(), &mut provider, Duration::ZERO),
                Decision::Suppress
            );
            assert!(!engine.is_composing());
        }
    }

    #[test]
    fn shift_passthrough_has_priority_over_normal_trigger_logic() {
        let mut engine = Engine::new(&config(IdleAction::None, true), Platform::Linux);
        let mut provider = MockImeStateProvider::new(ImeGuess::Yes, None);
        let event = ObservedEvent::TriggerKeyDown {
            shift: true,
            other_mods: false,
            repeat: false,
            self_injected: false,
        };

        assert_eq!(
            engine.handle_event(event, &mut provider, Duration::ZERO),
            Decision::PassThroughCapsLock
        );
        assert_eq!(provider.calls, 0);
    }

    #[test]
    fn shift_does_not_passthrough_when_disabled() {
        let mut engine = Engine::new(&config(IdleAction::None, false), Platform::Linux);
        let mut provider = MockImeStateProvider::new(ImeGuess::Yes, None);
        let event = ObservedEvent::TriggerKeyDown {
            shift: true,
            other_mods: false,
            repeat: false,
            self_injected: false,
        };

        assert_eq!(
            engine.handle_event(event, &mut provider, Duration::ZERO),
            Decision::Suppress
        );
    }

    #[test]
    fn other_modifiers_suppress_without_querying_ime() {
        let mut engine = Engine::new(&config(IdleAction::CapsLock, true), Platform::Linux);
        let mut provider = MockImeStateProvider::new(ImeGuess::Yes, None);
        let event = ObservedEvent::TriggerKeyDown {
            shift: true,
            other_mods: true,
            repeat: false,
            self_injected: false,
        };

        assert_eq!(
            engine.handle_event(event, &mut provider, Duration::ZERO),
            Decision::Suppress
        );
        assert_eq!(provider.calls, 0);
    }

    #[test]
    fn repeated_trigger_is_suppressed_without_action() {
        let mut engine = Engine::new(&config(IdleAction::CapsLock, true), Platform::Linux);
        let mut provider = MockImeStateProvider::new(ImeGuess::Yes, None);
        let event = ObservedEvent::TriggerKeyDown {
            shift: false,
            other_mods: false,
            repeat: true,
            self_injected: false,
        };

        assert_eq!(
            engine.handle_event(event, &mut provider, Duration::ZERO),
            Decision::Suppress
        );
        assert_eq!(provider.calls, 0);
    }

    #[test]
    fn repeated_printable_does_not_start_or_extend_composition() {
        let mut engine = Engine::new(&config(IdleAction::None, true), Platform::Linux);
        let mut provider = MockImeStateProvider::new(ImeGuess::Yes, None);
        let repeated = ObservedEvent::PrintableKeyDown {
            repeat: true,
            self_injected: false,
        };

        assert_eq!(
            engine.handle_event(repeated, &mut provider, Duration::ZERO),
            Decision::Ignore
        );
        assert!(!engine.is_composing());
        assert_eq!(provider.calls, 0);
    }

    #[test]
    fn repeated_commit_like_key_does_not_change_tracker() {
        let mut engine = Engine::new(&config(IdleAction::None, true), Platform::Linux);
        let mut provider = MockImeStateProvider::new(ImeGuess::Yes, None);
        start_composing(&mut engine, &mut provider);
        let repeated = ObservedEvent::CommitLikeKeyDown {
            repeat: true,
            self_injected: false,
        };

        assert_eq!(
            engine.handle_event(repeated, &mut provider, Duration::from_secs(1)),
            Decision::Ignore
        );
        assert!(engine.is_composing());
    }

    #[test]
    fn platform_uncertainty_reset_clears_composition() {
        let mut engine = Engine::new(&config(IdleAction::None, true), Platform::Windows);
        let mut provider = MockImeStateProvider::new(ImeGuess::Yes, None);
        start_composing(&mut engine, &mut provider);

        engine.reset_composition();

        assert!(!engine.is_composing());
    }

    #[test]
    fn self_injected_printable_clears_tracker_and_is_ignored() {
        let mut engine = Engine::new(&config(IdleAction::None, true), Platform::Linux);
        let mut provider = MockImeStateProvider::new(ImeGuess::Yes, None);
        start_composing(&mut engine, &mut provider);
        let injected_ctrl_m_m = ObservedEvent::PrintableKeyDown {
            repeat: false,
            self_injected: true,
        };

        assert_eq!(
            engine.handle_event(injected_ctrl_m_m, &mut provider, Duration::from_secs(1)),
            Decision::Ignore
        );
        assert!(!engine.is_composing());
    }

    #[test]
    fn self_injected_commit_clears_tracker_and_is_ignored() {
        let mut engine = Engine::new(&config(IdleAction::None, true), Platform::Linux);
        let mut provider = MockImeStateProvider::new(ImeGuess::Yes, None);
        start_composing(&mut engine, &mut provider);
        let injected = ObservedEvent::CommitLikeKeyDown {
            repeat: false,
            self_injected: true,
        };

        assert_eq!(
            engine.handle_event(injected, &mut provider, Duration::from_secs(1)),
            Decision::Ignore
        );
        assert!(!engine.is_composing());
    }

    #[test]
    fn auto_uses_ctrl_m_for_windows_allowlist() {
        for ime_id in WINDOWS_CTRL_M_IME_IDS {
            assert_eq!(
                resolve_commit_key(CommitKeyConfig::Auto, Some(ime_id), Platform::Windows),
                CommitKey::CtrlM
            );
        }
    }

    #[test]
    fn auto_uses_ctrl_m_for_linux_mozc_allowlist() {
        for ime_id in LINUX_CTRL_M_IME_IDS {
            assert_eq!(
                resolve_commit_key(CommitKeyConfig::Auto, Some(ime_id), Platform::Linux),
                CommitKey::CtrlM
            );
        }
    }

    #[test]
    fn auto_falls_back_to_enter_for_unlisted_or_missing_id() {
        for platform in [Platform::Windows, Platform::MacOs, Platform::Linux] {
            for ime_id in [Some("unlisted-ime"), None] {
                assert_eq!(
                    resolve_commit_key(CommitKeyConfig::Auto, ime_id, platform),
                    CommitKey::Enter
                );
            }
        }
    }

    #[test]
    fn explicit_commit_key_has_priority_over_allowlist() {
        assert_eq!(
            resolve_commit_key(CommitKeyConfig::Enter, Some("ms-ime"), Platform::Windows),
            CommitKey::Enter
        );
        assert_eq!(
            resolve_commit_key(
                CommitKeyConfig::CtrlM,
                Some("unlisted-ime"),
                Platform::Linux
            ),
            CommitKey::CtrlM
        );
        assert_eq!(
            resolve_commit_key(CommitKeyConfig::CtrlM, None, Platform::MacOs),
            CommitKey::CtrlM
        );
    }

    #[test]
    fn macos_auto_allowlist_is_empty() {
        for ime_id in ["com.apple.inputmethod.Japanese", "google-japanese-input"] {
            assert_eq!(
                resolve_commit_key(CommitKeyConfig::Auto, Some(ime_id), Platform::MacOs),
                CommitKey::Enter
            );
        }
    }
}
