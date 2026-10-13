//! The built-in actions a chord or the app menu runs ([`crate::app_actions`]): gated by the palette's enablement,
//! the model work done through the control handlers, the rest left to the frontend.

use agterm_proto::Args;
use agterm_proto::command::SessionCmd;

use super::{Fixture, wire};
use crate::app_actions::{ActionEffect, ActionOrigin, FrontendAction};
use crate::keymap::BuiltinAction;
use crate::palette_catalog::{PaletteCommand, PaletteFacts};

fn origin(fixture: &Fixture) -> ActionOrigin {
    ActionOrigin {
        window: fixture.window,
        quick: false,
    }
}

fn perform(fixture: &mut Fixture, action: BuiltinAction) -> ActionEffect {
    let at = origin(fixture);
    let effect = fixture.core.perform_builtin(at, action);
    let _ = fixture.core.commit();
    effect
}

#[test]
fn the_context_reads_the_window() {
    let mut fixture = Fixture::new(1);
    let context = fixture.core.palette_context(fixture.window);
    assert!(context.has(PaletteFacts::HAS_ACTIVE_SESSION | PaletteFacts::HAS_CURRENT_WORKSPACE));
    assert!(!context.has(PaletteFacts::CAN_STEP_WINDOWS));
    assert!(!context.modal_active());
    let session = fixture.sessions[0];
    assert!(
        fixture
            .session(SessionCmd::Flag, Some(&wire(session)), Args::default())
            .ok
    );
    assert!(
        fixture
            .core
            .palette_context(fixture.window)
            .has(PaletteFacts::HAS_FLAGGED_SESSIONS)
    );
}

#[test]
fn new_session_and_navigation_act_on_the_window() {
    let mut fixture = Fixture::new(1);
    let before = fixture.order(fixture.workspace).len();
    assert_eq!(perform(&mut fixture, BuiltinAction::NewSession), ActionEffect::Done);
    assert_eq!(fixture.order(fixture.workspace).len(), before + 1);
    perform(&mut fixture, BuiltinAction::FirstSession);
    assert_eq!(fixture.selected(), Some(fixture.order(fixture.workspace)[0]));
    perform(&mut fixture, BuiltinAction::NextSession);
    assert_eq!(fixture.selected(), Some(fixture.order(fixture.workspace)[1]));
    perform(&mut fixture, BuiltinAction::LastSession);
    assert_eq!(fixture.selected(), fixture.order(fixture.workspace).last().copied());
}

#[test]
fn split_scratch_flag_and_sidebar_toggle_the_model() {
    let mut fixture = Fixture::new(0);
    let session = fixture.selected().unwrap();
    perform(&mut fixture, BuiltinAction::ToggleSplit);
    assert!(fixture.store().session(session).unwrap().split.shown);
    perform(&mut fixture, BuiltinAction::ToggleScratch);
    assert!(fixture.store().session(session).unwrap().scratch_active);
    // a shown scratch is dismissed first, so the split stays
    perform(&mut fixture, BuiltinAction::ToggleHorizontalSplit);
    let state = fixture.store().session(session).unwrap();
    assert!(!state.scratch_active && state.split.shown);
    perform(&mut fixture, BuiltinAction::ToggleFlag);
    assert!(fixture.store().session(session).unwrap().flagged);
    let visible = fixture.store().sidebar().visible;
    perform(&mut fixture, BuiltinAction::ToggleSidebar);
    assert_ne!(fixture.store().sidebar().visible, visible);
}

#[test]
fn dialogs_and_later_uis_are_left_to_the_frontend() {
    let mut fixture = Fixture::new(0);
    let window = fixture.window;
    let session = fixture.selected().unwrap();
    assert_eq!(
        perform(&mut fixture, BuiltinAction::RenameSession),
        ActionEffect::Frontend(FrontendAction::RenameSession { window, session })
    );
    assert_eq!(
        perform(&mut fixture, BuiltinAction::OpenDirectory),
        ActionEffect::Frontend(FrontendAction::OpenDirectory {
            window,
            workspace: fixture.workspace
        })
    );
    assert_eq!(
        perform(&mut fixture, BuiltinAction::CommandPalette),
        ActionEffect::Frontend(FrontendAction::CommandPalette(window))
    );
    assert_eq!(
        perform(&mut fixture, BuiltinAction::ToggleFullscreen),
        ActionEffect::Frontend(FrontendAction::ToggleFullscreen(window))
    );
    // the last window cannot be deleted
    assert_eq!(perform(&mut fixture, BuiltinAction::DeleteWindow), ActionEffect::Done);
}

#[test]
fn close_session_closes_the_session_then_the_window() {
    let mut fixture = Fixture::new(1);
    let window = fixture.window;
    let first = fixture.selected().unwrap();
    assert_eq!(perform(&mut fixture, BuiltinAction::CloseSession), ActionEffect::Done);
    assert!(fixture.store().session(first).is_none());
    perform(&mut fixture, BuiltinAction::CloseSession);
    assert_eq!(
        perform(&mut fixture, BuiltinAction::CloseSession),
        ActionEffect::Frontend(FrontendAction::CloseWindow(window))
    );
}

#[test]
fn close_session_takes_a_cover_down_first() {
    let mut fixture = Fixture::new(0);
    let session = fixture.selected().unwrap();
    perform(&mut fixture, BuiltinAction::ToggleScratch);
    perform(&mut fixture, BuiltinAction::CloseSession);
    let state = fixture.store().session(session).unwrap();
    assert!(!state.scratch_active, "the scratch went, the session stays");
}

#[test]
fn a_dashboard_cover_disables_the_ordinary_actions() {
    let mut fixture = Fixture::new(1);
    fixture.core.toggle_dashboard(fixture.window);
    if fixture.core.dashboard(fixture.window).is_none() {
        return;
    }
    let before = fixture.order(fixture.workspace).len();
    perform(&mut fixture, BuiltinAction::NewSession);
    assert_eq!(fixture.order(fixture.workspace).len(), before);
    // its own chord still closes it
    perform(&mut fixture, BuiltinAction::Dashboard);
    assert!(fixture.core.dashboard(fixture.window).is_none());
}

#[test]
fn menu_only_rows_run_through_the_store() {
    let mut fixture = Fixture::new(1);
    for session in fixture.sessions.clone() {
        assert!(
            fixture
                .session(SessionCmd::Flag, Some(&wire(session)), Args::default())
                .ok
        );
    }
    let window = fixture.window;
    assert_eq!(
        fixture.core.perform_menu_command(window, PaletteCommand::ClearFlagged),
        ActionEffect::Done
    );
    assert!(fixture.store().flagged_sessions().is_empty());
    // a built-in row goes through the built-in path
    let before = fixture.order(fixture.workspace).len();
    fixture.core.perform_menu_command(window, PaletteCommand::NewSession);
    assert_eq!(fixture.order(fixture.workspace).len(), before + 1);
}

#[test]
fn every_builtin_reaches_an_effect_without_panicking() {
    for action in BuiltinAction::all() {
        let mut fixture = Fixture::new(2);
        let _ = perform(&mut fixture, action);
    }
}
