use std::time::Duration;

use cl4se::{
    config::{Config, IdleAction},
    core::{CommitKey, Decision, Engine, ImeGuess, ImeSnapshot, ObservedEvent, Platform},
    platform::ImeStateProvider,
};

struct MockImeStateProvider {
    snapshot: ImeSnapshot,
}

fn shift_enter_config() -> Config {
    let mut config = Config::default();
    config.general.idle_action = IdleAction::ShiftEnter;
    config
}

impl MockImeStateProvider {
    fn japanese_ime() -> Self {
        Self {
            snapshot: ImeSnapshot {
                active: ImeGuess::Yes,
                ime_id: Some("unlisted-test-ime".to_owned()),
            },
        }
    }
}

impl ImeStateProvider for MockImeStateProvider {
    fn snapshot(&mut self) -> ImeSnapshot {
        self.snapshot.clone()
    }
}

fn printable() -> ObservedEvent {
    ObservedEvent::PrintableKeyDown {
        repeat: false,
        self_injected: false,
    }
}

fn caps_lock() -> ObservedEvent {
    ObservedEvent::TriggerKeyDown {
        shift: false,
        other_mods: false,
        repeat: false,
        self_injected: false,
    }
}

fn injected_commit() -> ObservedEvent {
    ObservedEvent::CommitLikeKeyDown {
        repeat: false,
        self_injected: true,
    }
}

#[test]
fn t1_japanese_input_then_caps_lock_commits() {
    let mut engine = Engine::new(&Config::default(), Platform::Linux);
    let mut ime = MockImeStateProvider::japanese_ime();

    assert_eq!(
        engine.handle_event(printable(), &mut ime, Duration::ZERO),
        Decision::Ignore
    );
    assert_eq!(
        engine.handle_event(caps_lock(), &mut ime, Duration::from_millis(10)),
        Decision::InjectCommitKey(CommitKey::Enter)
    );
}

#[test]
fn t2_caps_lock_immediately_after_commit_does_nothing() {
    let mut engine = Engine::new(&Config::default(), Platform::Linux);
    let mut ime = MockImeStateProvider::japanese_ime();
    engine.handle_event(printable(), &mut ime, Duration::ZERO);
    engine.handle_event(caps_lock(), &mut ime, Duration::from_millis(10));

    assert_eq!(
        engine.handle_event(injected_commit(), &mut ime, Duration::from_millis(11)),
        Decision::Ignore
    );
    assert_eq!(
        engine.handle_event(caps_lock(), &mut ime, Duration::from_millis(12)),
        Decision::Suppress
    );
}

#[test]
fn t4_caps_lock_after_click_does_nothing() {
    let mut engine = Engine::new(&Config::default(), Platform::Linux);
    let mut ime = MockImeStateProvider::japanese_ime();
    engine.handle_event(printable(), &mut ime, Duration::ZERO);

    assert_eq!(
        engine.handle_event(
            ObservedEvent::MouseClick,
            &mut ime,
            Duration::from_millis(10)
        ),
        Decision::Ignore
    );
    assert_eq!(
        engine.handle_event(caps_lock(), &mut ime, Duration::from_millis(11)),
        Decision::Suppress
    );
}

#[test]
fn t10_opt_in_shift_enter_runs_only_for_known_non_composing_state() {
    for active in [ImeGuess::Yes, ImeGuess::No] {
        let mut engine = Engine::new(&shift_enter_config(), Platform::Linux);
        let mut ime = MockImeStateProvider {
            snapshot: ImeSnapshot {
                active,
                ime_id: None,
            },
        };

        assert_eq!(
            engine.handle_event(caps_lock(), &mut ime, Duration::ZERO),
            Decision::InjectShiftEnter
        );
    }

    let mut engine = Engine::new(&shift_enter_config(), Platform::Linux);
    let mut unknown_ime = MockImeStateProvider {
        snapshot: ImeSnapshot {
            active: ImeGuess::Unknown,
            ime_id: None,
        },
    };
    assert_eq!(
        engine.handle_event(caps_lock(), &mut unknown_ime, Duration::ZERO),
        Decision::Suppress
    );
}
