//! Keys → [`UiAction`]s. Every context rule lives here: which modal is open,
//! the report stage, the setup-number chord, and the text fields.

use crate::{
    app::{filtered_roster, report_roster, AppState, Modal, ReportDraft, ReportStage, SELECT_CALLED_SETUP_FIRST},
    config::SetupId,
    ui_action::{Move, ReportAction, Side, UiAction},
};

/// The keys the scheduler reacts to, independent of the terminal library.
/// `Other` is any key it doesn't model (those still dismiss list modals).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Key {
    Char(char),
    Enter,
    Esc,
    Backspace,
    Tab,
    Up,
    Down,
    PageUp,
    PageDown,
    CtrlC,
    Other,
}

#[derive(Debug, Clone, PartialEq)]
pub enum KeyOutcome {
    Action(UiAction),
    /// The key needs context the desk hasn't given (shown as a warning).
    Reject(&'static str),
    Ignore,
}

/// What a key means right now, given the open modal, report stage, pending
/// setup digits and text fields.
pub fn resolve_key(state: &AppState, key: Key) -> KeyOutcome {
    if key == Key::CtrlC {
        return KeyOutcome::Action(UiAction::Quit);
    }
    match &state.ui.modal {
        Some(modal) => resolve_modal(state, modal, key),
        None => resolve_main(state, key),
    }
}

fn resolve_main(state: &AppState, key: Key) -> KeyOutcome {
    let action = match key {
        Key::Char('q') => UiAction::Quit,
        Key::Char('?') => UiAction::OpenHelp,
        Key::Char(c @ '0'..='9') => setup_digit(state, c),
        Key::Enter => match &state.ui.setup_entry {
            Some(pending) => UiAction::SelectSetup(SetupId(parse_setup_digits(&pending.digits))),
            None => UiAction::QuickCall(state.ui.queue_ix),
        },
        Key::Esc => UiAction::ClearSetupEntry,
        Key::Char('p') => return on_selected_setup(state, UiAction::Progress, SELECT_CALLED_SETUP_FIRST),
        Key::Char('f') => return on_selected_setup(state, UiAction::Free, "select a setup first (digit), then f"),
        Key::Char('r') => return on_selected_setup(state, UiAction::Requeue, "select a setup first (digit), then r"),
        Key::Char('z') => UiAction::Snooze(state.ui.queue_ix),
        Key::Char('u') => UiAction::Undo,
        Key::Char('s') => UiAction::OpenSetups,
        Key::Char('i') => UiAction::OpenInspection,
        Key::Char('n') => UiAction::OpenNotices,
        Key::Char('w') => UiAction::OpenPendingWrites,
        Key::Char('d') => UiAction::OpenFlags(state.ui.queue_ix),
        Key::Char('a') => return on_selected_setup(state, UiAction::OpenReassign, "select a setup first (digit), then a"),
        Key::Char('g') => return on_selected_setup(state, UiAction::OpenReport, "select a setup first (digit), then g"),
        Key::Char('/') => UiAction::OpenFindSet,
        Key::Char('t') => UiAction::ToggleSponsors,
        Key::Up => UiAction::MoveQueueCursor(Move::Up),
        Key::Down => UiAction::MoveQueueCursor(Move::Down),
        Key::PageUp => UiAction::MoveQueueCursor(Move::PageUp),
        Key::PageDown => UiAction::MoveQueueCursor(Move::PageDown),
        _ => return KeyOutcome::Ignore,
    };
    KeyOutcome::Action(action)
}

/// The hot keys act on the selected setup; without a live one (none, or a
/// station since retired) they explain how to select it.
fn on_selected_setup(state: &AppState, action: fn(SetupId) -> UiAction, reject: &'static str) -> KeyOutcome {
    let live = state
        .ui
        .selected_setup
        .filter(|setup| state.board.setups().iter().any(|s| s.id == *setup));
    match live {
        Some(setup) => KeyOutcome::Action(action(setup)),
        None => KeyOutcome::Reject(reject),
    }
}

/// A digit either completes a setup number or, on a board past ten
/// stations where a second digit could still follow, waits for it.
fn setup_digit(state: &AppState, digit: char) -> UiAction {
    let max_station = state.board.setups().iter().map(|s| s.id.0).max().unwrap_or(0);
    let mut digits = state.ui.setup_entry.as_ref().map(|p| p.digits.clone()).unwrap_or_default();
    digits.push(digit);
    let number = parse_setup_digits(&digits);
    if max_station > 10 && number != 10 && number.saturating_mul(10) <= max_station {
        UiAction::BufferSetupDigits(digits)
    } else {
        UiAction::SelectSetup(SetupId(number))
    }
}

/// A lone `0` keeps meaning setup 10.
pub(crate) fn parse_setup_digits(digits: &str) -> u32 {
    match digits.parse().unwrap_or(0) {
        0 if digits.len() == 1 => 10,
        n => n,
    }
}

fn resolve_modal(state: &AppState, modal: &Modal, key: Key) -> KeyOutcome {
    if key == Key::Esc {
        // The report modal steps back a stage instead of losing the draft.
        return KeyOutcome::Action(match modal {
            Modal::Report(draft) if draft.stage != ReportStage::Games => UiAction::Report(ReportAction::Back),
            _ => UiAction::CloseModal,
        });
    }
    let action = match modal {
        Modal::CallPicker { setup, selected, .. } => match key {
            Key::Up => UiAction::MoveModalCursor(Move::Up),
            Key::Down => UiAction::MoveModalCursor(Move::Down),
            Key::Enter => UiAction::Call {
                setup: *setup,
                candidate: *selected,
            },
            // An exhausted pool presents an empty picker; `a` jumps
            // straight to reassignment for the same setup.
            Key::Char('a') => UiAction::OpenReassign(*setup),
            _ => return KeyOutcome::Ignore,
        },
        Modal::Inspection { .. } => return list_key(key),
        Modal::Notices { selected } => match key {
            Key::Enter => UiAction::AckNotice(*selected),
            Key::Char('c') => UiAction::ClearNotices,
            _ => return list_key(key),
        },
        Modal::PendingWrites { selected } => match key {
            Key::Enter => UiAction::RetryParked(*selected),
            Key::Char('d') => UiAction::DiscardPending(*selected),
            _ => return list_key(key),
        },
        Modal::PlayerFlags { selected, .. } => match key {
            Key::Enter => UiAction::CycleFlag(*selected),
            _ => return list_key(key),
        },
        Modal::Reassign { setup, selected } => match key {
            Key::Enter => UiAction::ApplyReassign {
                setup: *setup,
                option: *selected,
            },
            _ => return list_key(key),
        },
        Modal::FindSet { query, selected } => match key {
            Key::Enter => UiAction::OpenFoundSet(*selected),
            Key::Char(c) => UiAction::SetFindQuery(format!("{query}{c}")),
            Key::Backspace => UiAction::SetFindQuery(without_last(query)),
            _ => return list_key(key),
        },
        Modal::Setups { selected } => {
            let entry = &state.ui.setups_count_entry;
            match key {
                Key::Char(c @ '0'..='9') if entry.len() < 3 => UiAction::SetSetupsCountEntry(format!("{entry}{c}")),
                Key::Backspace if !entry.is_empty() => UiAction::SetSetupsCountEntry(without_last(entry)),
                Key::Enter => match entry.parse::<u32>() {
                    Ok(target) => UiAction::ApplySetupsCount { row: *selected, target },
                    Err(_) => UiAction::ApplySetupsRow(*selected),
                },
                _ => return list_key(key),
            }
        }
        Modal::Report(draft) => return report_key(state, draft, key),
        Modal::Help => UiAction::CloseModal,
    };
    KeyOutcome::Action(action)
}

/// Shared list-modal rule: Up/Down/PgUp/PgDn move, anything else closes.
fn list_key(key: Key) -> KeyOutcome {
    KeyOutcome::Action(match key {
        Key::Up => UiAction::MoveModalCursor(Move::Up),
        Key::Down => UiAction::MoveModalCursor(Move::Down),
        Key::PageUp => UiAction::MoveModalCursor(Move::PageUp),
        Key::PageDown => UiAction::MoveModalCursor(Move::PageDown),
        _ => UiAction::CloseModal,
    })
}

fn report_key(state: &AppState, draft: &ReportDraft, key: Key) -> KeyOutcome {
    let action = match &draft.stage {
        ReportStage::Games => match key {
            Key::Char('1') => ReportAction::RecordGame(Side::Left),
            Key::Char('2') => ReportAction::RecordGame(Side::Right),
            Key::Backspace => ReportAction::UndoGame,
            Key::Up => ReportAction::MoveGameCursor(Move::Up),
            Key::Down => ReportAction::MoveGameCursor(Move::Down),
            Key::Char('c') => ReportAction::OpenCharacterPicker,
            Key::Char('d') => ReportAction::StartDq,
            Key::Enter => ReportAction::FinishGames,
            _ => return KeyOutcome::Ignore,
        },
        ReportStage::Characters { filter, cursor, .. } => match key {
            Key::Enter => {
                let choice = filtered_roster(report_roster(state, &draft.bracket), filter)
                    .get(*cursor)
                    .map(|c| c.id);
                ReportAction::PickCharacter(choice)
            }
            // Tab keeps whatever the side already had (sticky or nothing).
            Key::Tab => ReportAction::PickCharacter(None),
            Key::Up => ReportAction::MoveCharacterCursor(Move::Up),
            Key::Down => ReportAction::MoveCharacterCursor(Move::Down),
            Key::Backspace => ReportAction::SetCharacterFilter(without_last(filter)),
            Key::Char(c) if c.is_ascii_alphanumeric() || matches!(c, ' ' | '-' | '.' | '&') => {
                ReportAction::SetCharacterFilter(format!("{filter}{c}"))
            }
            _ => return KeyOutcome::Ignore,
        },
        ReportStage::DqPick => match key {
            Key::Char('1') => ReportAction::PickDq(Side::Left),
            Key::Char('2') => ReportAction::PickDq(Side::Right),
            _ => return KeyOutcome::Ignore,
        },
        ReportStage::Confirm { .. } => match key {
            Key::Enter | Key::Char('y') => ReportAction::Submit,
            _ => return KeyOutcome::Ignore,
        },
    };
    KeyOutcome::Action(UiAction::Report(action))
}

fn without_last(text: &str) -> String {
    let mut text = text.to_owned();
    text.pop();
    text
}

#[cfg(test)]
mod tests {
    use super::{resolve_key, Key, KeyOutcome};
    use crate::{
        app::{AppState, BracketBootstrap, Modal, PendingSetupEntry},
        config::{BracketConfig, BracketMode, SchedulerConfig, SetupCounts, SetupId, DEFAULT_SETUP_TYPE},
        model::BracketId,
        synth::{make_se_bracket, materialize_ids},
        ui_action::{Move, UiAction},
    };

    const NOW: i64 = 1_751_000_000_000;

    fn app(stations: u32) -> AppState {
        let config = SchedulerConfig {
            setups: Some(SetupCounts::Uniform(stations)),
            brackets: vec![BracketConfig::new("ultimate")],
            ..SchedulerConfig::default()
        };
        let mut bracket = make_se_bracket(1001, 4);
        bracket.sets = materialize_ids(&bracket.sets, 9000);
        let boot = BracketBootstrap {
            id: BracketId("ultimate".to_owned()),
            sets: bracket.sets,
            groups: vec![bracket.info],
            mode: BracketMode::Full,
            start_at: None,
            setup_types: vec![DEFAULT_SETUP_TYPE.to_owned()],
            duration_prior_secs: 480,
            prior_weight: 4.0,
            characters: Vec::new(),
        };
        AppState::new(config, true, vec![boot], NOW)
    }

    fn action(state: &AppState, key: Key) -> UiAction {
        match resolve_key(state, key) {
            KeyOutcome::Action(action) => action,
            other => panic!("expected an action, got {other:?}"),
        }
    }

    #[test]
    fn ctrl_c_quits_even_inside_a_modal() {
        let mut state = app(2);
        state.ui.modal = Some(Modal::Notices { selected: 0 });
        assert_eq!(action(&state, Key::CtrlC), UiAction::Quit);
    }

    #[test]
    fn unmodelled_keys_dismiss_list_modals_but_not_the_picker() {
        let mut state = app(2);
        state.ui.modal = Some(Modal::Notices { selected: 0 });
        assert_eq!(action(&state, Key::Other), UiAction::CloseModal);
        assert_eq!(action(&state, Key::PageDown), UiAction::MoveModalCursor(Move::PageDown));
        state.ui.modal = Some(Modal::CallPicker {
            setup: SetupId(1),
            selected: 0,
            refreshed: false,
        });
        assert_eq!(resolve_key(&state, Key::Other), KeyOutcome::Ignore);
    }

    #[test]
    fn hot_keys_need_a_live_selection() {
        let mut state = app(2);
        assert_eq!(
            resolve_key(&state, Key::Char('f')),
            KeyOutcome::Reject("select a setup first (digit), then f")
        );
        state.ui.selected_setup = Some(SetupId(9));
        assert_eq!(
            resolve_key(&state, Key::Char('r')),
            KeyOutcome::Reject("select a setup first (digit), then r")
        );
        state.ui.selected_setup = Some(SetupId(2));
        assert_eq!(action(&state, Key::Char('g')), UiAction::OpenReport(SetupId(2)));
    }

    #[test]
    fn digits_chord_only_on_boards_past_ten() {
        let small = app(2);
        assert_eq!(action(&small, Key::Char('0')), UiAction::SelectSetup(SetupId(10)));
        assert_eq!(action(&small, Key::Char('1')), UiAction::SelectSetup(SetupId(1)));
        let mut big = app(12);
        assert_eq!(action(&big, Key::Char('1')), UiAction::BufferSetupDigits("1".to_owned()));
        big.ui.setup_entry = Some(PendingSetupEntry {
            digits: "1".to_owned(),
            at: NOW,
        });
        assert_eq!(action(&big, Key::Char('2')), UiAction::SelectSetup(SetupId(12)));
        assert_eq!(action(&big, Key::Enter), UiAction::SelectSetup(SetupId(1)));
    }

    #[test]
    fn text_fields_emit_the_whole_new_value() {
        let mut state = app(2);
        state.ui.modal = Some(Modal::FindSet {
            query: "ab".to_owned(),
            selected: 3,
        });
        assert_eq!(action(&state, Key::Backspace), UiAction::SetFindQuery("a".to_owned()));
        assert_eq!(action(&state, Key::Char('c')), UiAction::SetFindQuery("abc".to_owned()));
        state.ui.modal = Some(Modal::Setups { selected: 1 });
        state.ui.setups_count_entry = "12".to_owned();
        assert_eq!(action(&state, Key::Enter), UiAction::ApplySetupsCount { row: 1, target: 12 });
        state.ui.setups_count_entry = "123".to_owned();
        assert_eq!(action(&state, Key::Char('4')), UiAction::CloseModal);
    }
}
