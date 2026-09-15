//! Tests for conversation rewind dispatchers and prompt-entry lookup.

use super::*;

#[test]
fn cancel_does_not_rewind_when_in_flight_block_committed() {
    // Minimal-mode regression: a user-prompt block commits to native scrollback immediately (it is never `is_running`)
    // A committed block can't be "un-printed", so cancelling must not rewind it
    // A rewind would `remove_entry` it from state while the printed copy stays on screen, then restore the text into the input, showing it twice
    let mut app = test_app_with_agent();
    let id = AgentId(0);

    dispatch(Action::SendPrompt("queued prompt".into()), &mut app);
    assert!(app.agents[&id].session.in_flight_prompt.is_some());
    assert_eq!(app.agents[&id].scrollback.len(), 1);

    // Simulate minimal's commit pass printing the user block into native scrollback (sets the entry's `committed` flag)
    let entry_id = app.agents[&id]
        .session
        .in_flight_prompt
        .as_ref()
        .unwrap()
        .scrollback_entry;
    let idx = app.agents[&id].scrollback.index_of_id(entry_id).unwrap();
    app.agents
        .get_mut(&id)
        .unwrap()
        .scrollback
        .mark_committed(idx);

    let effects = dispatch(Action::CancelTurn, &mut app);
    assert_eq!(effects.len(), 1);
    assert!(matches!(&effects[0], Effect::CancelTurn { .. }));

    // Standard cancel, not the rewind: the prompt is not restored to the input and the committed block stays in scrollback (no duplicate)
    assert!(
        app.agents[&id].prompt.text().is_empty(),
        "committed in-flight block must not be rewound into the input"
    );
    assert_eq!(
        app.agents[&id].scrollback.len(),
        1,
        "committed block must stay in scrollback (it's already printed)"
    );
    assert!(app.agents[&id].session.state.is_cancelling());
}

#[test]
fn rewind_then_resubmit_drains_immediately_and_discards_orphan() {
    // After a rewind, state is Idle so a follow-up prompt can drain without waiting for the cancelled turn's PromptResponse
    let mut app = test_app_with_agent();
    let id = AgentId(0);

    dispatch(Action::SendPrompt("first".into()), &mut app);
    let first_pid = app.agents[&id].session.current_prompt_id.clone();
    assert!(first_pid.is_some());
    dispatch(Action::CancelTurn, &mut app);
    assert!(app.agents[&id].session.state.is_idle());
    assert!(app.agents[&id].session.current_prompt_id.is_none());

    // User edits and re-submits without waiting.
    let effects = dispatch(Action::SendPrompt("second".into()), &mut app);
    assert_eq!(effects.len(), 1);
    assert!(matches!(&effects[0], Effect::SendPrompt { text, .. } if text == "second"));
    assert!(app.agents[&id].session.state.is_turn_running());
    let second_pid = app.agents[&id].session.current_prompt_id.clone();
    assert!(second_pid.is_some());
    assert_ne!(first_pid, second_pid);

    // The cancelled "first" PromptResponse arrives mid-second-turn, carrying first_pid
    // It mismatches current_prompt_id (second_pid), so it is discarded; state for "second" is untouched
    dispatch(
        Action::TaskComplete(TaskResult::PromptResponse {
            agent_id: id,
            result: Ok(acp::PromptResponse::new(acp::StopReason::Cancelled).meta(
                serde_json::json!({ "promptId": first_pid })
                    .as_object()
                    .cloned(),
            )),
            http_status: None,
            prompt_id: None,
        }),
        &mut app,
    );
    assert!(app.agents[&id].session.state.is_turn_running());
    assert_eq!(app.agents[&id].session.current_prompt_id, second_pid);
}

/// Ctrl+C rewind cancel carries the rewound turn's prompt id.
#[test]
fn cancel_rewind_effect_carries_the_rewound_prompt_id() {
    let mut app = test_app_with_agent();
    let id = AgentId(0);

    dispatch(Action::SendPrompt("rewind me".into()), &mut app);
    let pid = app.agents[&id]
        .session
        .current_prompt_id
        .clone()
        .expect("turn running");

    let effects = dispatch(Action::CancelTurn, &mut app);
    assert!(
        matches!(
            effects.as_slice(),
            [Effect::CancelTurn {
                rewind_prompt_id: Some(p),
                ..
            }] if *p == pid
        ),
        "rewind cancel must carry the captured prompt id, got {effects:?}"
    );
    let agent = &app.agents[&id];
    assert!(agent.session.state.is_idle());
    assert_eq!(agent.prompt.text(), "rewind me");
    assert!(agent.is_rewound_prompt(&pid));
}

/// No prompt id means no optimistic rewind; send a standard cancel.
#[test]
fn cancel_without_prompt_id_skips_rewind_and_sends_normal_cancel() {
    let mut app = test_app_with_agent();
    let id = AgentId(0);

    dispatch(Action::SendPrompt("cannot rewind".into()), &mut app);
    assert!(app.agents[&id].session.in_flight_prompt.is_some());
    // Simulate the id being gone while the stash survives.
    app.agents.get_mut(&id).unwrap().session.current_prompt_id = None;

    let effects = dispatch(Action::CancelTurn, &mut app);
    assert!(
        matches!(
            effects.as_slice(),
            [Effect::CancelTurn {
                rewind_prompt_id: None,
                ..
            }]
        ),
        "id-less cancel must not request a rewind, got {effects:?}"
    );
    let agent = &app.agents[&id];
    assert!(
        agent.prompt.text().is_empty(),
        "no optimistic composer restore without an id"
    );
    assert_eq!(
        agent.scrollback.len(),
        1,
        "the prompt block stays in scrollback (standard cancel)"
    );
    assert!(agent.session.state.is_cancelling());
}

/// Set up an app whose agent has one user prompt and one agent message in the transcript.
/// The agent is inline-editing that prompt with `edited` typed in, and an unrelated draft sits in the composer.
fn app_mid_inline_edit(edited: &str) -> AppView {
    let mut app = test_app_with_agent();
    let id = AgentId(0);
    let agent = app.agents.get_mut(&id).unwrap();
    agent
        .scrollback
        .push_block(RenderBlock::user_prompt("fix the bug"));
    agent
        .scrollback
        .push_block(RenderBlock::agent_message("done"));
    agent.scrollback.prepare_layout(80, 40);
    assert!(agent.enter_inline_edit(0));
    agent
        .inline_edit
        .as_mut()
        .unwrap()
        .textarea
        .set_text(edited);
    agent.prompt.set_text("composer draft");
    app
}

/// Rewind point for the fixture's single prompt.
fn rewind_point(prompt_index: usize) -> crate::views::rewind::RewindPointInfo {
    crate::views::rewind::RewindPointInfo {
        prompt_index,
        created_at: String::new(),
        num_file_snapshots: 0,
        prompt_preview: Some("fix the bug".into()),
        has_file_changes: false,
    }
}

/// Rewind point carrying `n` tracked file snapshots of its own.
fn rewind_point_with_files(prompt_index: usize, n: usize) -> crate::views::rewind::RewindPointInfo {
    crate::views::rewind::RewindPointInfo {
        prompt_index,
        created_at: String::new(),
        num_file_snapshots: n,
        prompt_preview: Some(format!("turn {prompt_index}")),
        has_file_changes: n > 0,
    }
}

/// Points-loaded task result carrying the fixture's single rewind point.
fn points_loaded(id: AgentId) -> Action {
    Action::TaskComplete(TaskResult::RewindPointsLoaded {
        agent_id: id,
        points: vec![rewind_point(0)],
    })
}

fn points_loaded_with(id: AgentId, points: Vec<crate::views::rewind::RewindPointInfo>) -> Action {
    Action::TaskComplete(TaskResult::RewindPointsLoaded {
        agent_id: id,
        points,
    })
}

/// A `force: false` dry-run response: the engine reports `success: false` on every preview,
/// with `error` set only when there are conflicts.
fn preview_response(
    target: usize,
    clean: &[&str],
    conflicts: &[(&str, &str)],
) -> crate::views::rewind::RewindResponse {
    crate::views::rewind::RewindResponse {
        success: false,
        target_prompt_index: target,
        reverted_files: vec![],
        clean_files: clean.iter().map(|s| (*s).to_string()).collect(),
        conflicts: conflicts
            .iter()
            .map(|(path, kind)| crate::views::rewind::RewindConflictInfo {
                path: (*path).to_string(),
                conflict_type: (*kind).to_string(),
            })
            .collect(),
        error: if conflicts.is_empty() {
            None
        } else {
            Some("External modifications detected. Confirm to revert anyway.".into())
        },
        mode: None,
        prompt_text: None,
    }
}

fn preview_complete(
    id: AgentId,
    target: usize,
    mode: crate::views::rewind::RewindMode,
    response: crate::views::rewind::RewindResponse,
) -> Action {
    Action::TaskComplete(TaskResult::RewindPreviewComplete {
        agent_id: id,
        response,
        target_prompt_index: target,
        mode,
    })
}

/// Successful rewind/execute response (conversation-only).
fn rewind_success(target: usize, prompt_text: &str) -> crate::views::rewind::RewindResponse {
    crate::views::rewind::RewindResponse {
        success: true,
        target_prompt_index: target,
        reverted_files: vec![],
        clean_files: vec![],
        conflicts: vec![],
        error: None,
        mode: Some("conversation_only".into()),
        prompt_text: Some(prompt_text.into()),
    }
}

/// Drive an idle inline-edit submit through to execute: the points fetch, then the confirm (setting on), then Executing.
/// Returns the effects of the confirm step.
fn drive_inline_submit_to_execute(app: &mut AppView) -> Vec<Effect> {
    let id = AgentId(0);
    let effects = dispatch(Action::InlineEditSubmit, app);
    assert!(
        matches!(&effects[0], Effect::FetchRewindPoints { .. }),
        "got {effects:?}"
    );
    dispatch(points_loaded(id), app);
    // The mode question comes first; inline resubmit hides the files-only row.
    assert!(matches!(
        app.agents[&id].rewind_state.as_ref().unwrap().phase,
        crate::views::rewind::RewindPhase::ModeSelect {
            offer_files_only: false,
            ..
        }
    ));
    dispatch(
        Action::RewindSelectMode {
            target: 0,
            mode: crate::views::rewind::RewindMode::ConversationOnly,
        },
        app,
    );
    // Confirm-before-rewind (default on) gates every conversation rewind, including at 0.
    assert!(matches!(
        app.agents[&id].rewind_state.as_ref().unwrap().phase,
        crate::views::rewind::RewindPhase::Confirm { .. }
    ));
    dispatch(Action::RewindConfirm(0), app)
}

/// Submitting an inline edit enters the exact same flow as `/rewind`: a Loading overlay and a points fetch pre-targeted at the edited prompt.
/// The editor stays open behind the flow and nothing is stashed yet.
#[test]
fn inline_edit_submit_enters_rewind_flow_via_points_fetch() {
    let mut app = app_mid_inline_edit("fix the bug properly");
    let id = AgentId(0);

    let effects = dispatch(Action::InlineEditSubmit, &mut app);

    assert_eq!(effects.len(), 1);
    assert!(matches!(&effects[0], Effect::FetchRewindPoints { .. }));
    let agent = &app.agents[&id];
    let state = agent.rewind_state.as_ref().expect("rewind flow entered");
    assert!(matches!(
        state.phase,
        crate::views::rewind::RewindPhase::Loading
    ));
    assert_eq!(
        state.selected_prompt_index,
        Some(0),
        "pre-targeted at the edited prompt"
    );
    assert!(agent.inline_edit.is_some(), "editor stays open");
    assert!(
        agent.pending_inline_resubmit.is_none(),
        "nothing stashed before an execute"
    );
}

/// A submit whose text is unchanged (or empty) has nothing to do: the editor just closes; no rewind flow, no effects.
#[test]
fn inline_edit_submit_with_unchanged_text_closes_editor() {
    let mut app = app_mid_inline_edit("fix the bug");
    let id = AgentId(0);

    let effects = dispatch(Action::InlineEditSubmit, &mut app);

    assert!(effects.is_empty());
    let agent = &app.agents[&id];
    assert!(agent.inline_edit.is_none(), "editor closed");
    assert!(agent.rewind_state.is_none(), "no rewind flow entered");
    assert!(agent.scrollback.inline_edit_height().is_none());
}

/// Points loaded with a pre-selected target skip the picker and open the mode question; the editor stays open behind it.
/// The files-only row is hidden inline, because the conversation rewind is a given there.
#[test]
fn inline_edit_points_loaded_opens_target_zero_mode_select_over_open_editor() {
    let mut app = app_mid_inline_edit("fix the bug properly");
    let id = AgentId(0);
    dispatch(Action::InlineEditSubmit, &mut app);

    let effects = dispatch(points_loaded(id), &mut app);
    assert!(
        effects.is_empty(),
        "the mode question waits for a pick, got {effects:?}"
    );

    let agent = &app.agents[&id];
    assert!(matches!(
        agent.rewind_state.as_ref().unwrap().phase,
        crate::views::rewind::RewindPhase::ModeSelect {
            target_prompt_index: 0,
            offer_files_only: false,
            ..
        }
    ));
    assert!(agent.inline_edit.is_some(), "editor still open");
    assert!(agent.pending_inline_resubmit.is_none());
}

/// Classic `/rewind` with a selected turn lands on the mode question, files-only row offered.
#[test]
fn classic_rewind_target_zero_opens_mode_select() {
    let mut app = test_app_with_agent();
    let id = AgentId(0);
    {
        let agent = app.agents.get_mut(&id).unwrap();
        agent
            .scrollback
            .push_block(RenderBlock::user_prompt("fix the bug"));
        agent
            .scrollback
            .push_block(RenderBlock::agent_message("done"));
        agent.scrollback.prepare_layout(80, 40);
        agent.scrollback.set_selected(Some(0));
    }

    let effects = dispatch(Action::Rewind, &mut app);
    assert!(
        matches!(&effects[0], Effect::FetchRewindPoints { .. }),
        "got {effects:?}"
    );
    let effects = dispatch(points_loaded(id), &mut app);
    assert!(
        effects.is_empty(),
        "the mode question waits for a pick, got {effects:?}"
    );

    assert!(matches!(
        app.agents[&id].rewind_state.as_ref().unwrap().phase,
        crate::views::rewind::RewindPhase::ModeSelect {
            target_prompt_index: 0,
            offer_files_only: true,
            has_file_changes: false,
            ..
        }
    ));
}

/// Settings action updates the live confirm-before-rewind value.
#[test]
fn set_confirm_before_rewind_updates_live_value() {
    let mut app = test_app_with_agent();
    assert!(app.current_ui.confirm_before_rewind_enabled());

    let effects = dispatch(Action::SetConfirmBeforeRewind(false), &mut app);
    assert!(
        matches!(
            &effects[0],
            Effect::PersistSetting {
                key: "confirm_before_rewind",
                value: crate::settings::SettingValue::Bool(false),
                ..
            }
        ),
        "got {effects:?}"
    );
    assert!(!app.current_ui.confirm_before_rewind_enabled());
    assert_eq!(app.current_ui.confirm_before_rewind, Some(false));

    let effects = dispatch(Action::SetConfirmBeforeRewind(false), &mut app);
    assert!(
        effects.is_empty(),
        "idempotent when already false, got {effects:?}"
    );
}

/// Multi-turn fixture with two user prompts for the picker and non-zero-target tests.
fn app_with_two_turns() -> AppView {
    let mut app = test_app_with_agent();
    let id = AgentId(0);
    {
        let agent = app.agents.get_mut(&id).unwrap();
        agent.session.session_id = Some(acp::SessionId::new("sess".to_string()));
        for i in 0..2 {
            let mut b = UserPromptBlock::new(format!("turn {i}"));
            b.prompt_index = Some(i);
            agent.scrollback.push_block(RenderBlock::UserPrompt(b));
            agent
                .scrollback
                .push_block(RenderBlock::agent_message("ok"));
        }
        agent.scrollback.prepare_layout(80, 40);
    }
    app
}

/// With confirm-before-rewind off, picking a turn opens mode select; choosing a mode executes.
#[test]
fn picker_select_nonzero_target_executes_immediately_when_confirm_off() {
    let mut app = app_with_two_turns();
    app.current_ui.confirm_before_rewind = Some(false);
    let id = AgentId(0);

    dispatch(Action::RewindShowPicker, &mut app);
    dispatch(
        Action::TaskComplete(TaskResult::RewindPointsLoaded {
            agent_id: id,
            points: vec![rewind_point(1), rewind_point(0)],
        }),
        &mut app,
    );
    assert!(matches!(
        app.agents[&id].rewind_state.as_ref().unwrap().phase,
        crate::views::rewind::RewindPhase::Picker { .. }
    ));

    let effects = dispatch(Action::RewindPickerSelect(1), &mut app);
    assert!(effects.is_empty(), "mode select first, got {effects:?}");
    assert!(matches!(
        app.agents[&id].rewind_state.as_ref().unwrap().phase,
        crate::views::rewind::RewindPhase::ModeSelect {
            target_prompt_index: 1,
            ..
        }
    ));

    // No tracked edits at or after turn 1, so "both" moves no files and needs no preview.
    let effects = dispatch(
        Action::RewindSelectMode {
            target: 1,
            mode: crate::views::rewind::RewindMode::All,
        },
        &mut app,
    );
    assert!(
        matches!(
            &effects[0],
            Effect::RewindExecute {
                target_prompt_index: 1,
                mode: crate::views::rewind::RewindMode::All,
                ..
            }
        ),
        "got {effects:?}"
    );
    assert!(matches!(
        app.agents[&id].rewind_state.as_ref().unwrap().phase,
        crate::views::rewind::RewindPhase::Executing {
            target_prompt_index: 1,
            mode: crate::views::rewind::RewindMode::All
        }
    ));
}

/// With confirm-before-rewind on (default), picking a turn opens mode select.
#[test]
fn picker_select_nonzero_target_opens_confirm_when_setting_on() {
    let mut app = app_with_two_turns();
    assert!(app.current_ui.confirm_before_rewind_enabled());
    let id = AgentId(0);

    dispatch(Action::RewindShowPicker, &mut app);
    dispatch(
        Action::TaskComplete(TaskResult::RewindPointsLoaded {
            agent_id: id,
            points: vec![rewind_point(1), rewind_point(0)],
        }),
        &mut app,
    );

    let effects = dispatch(Action::RewindPickerSelect(1), &mut app);
    assert!(effects.is_empty(), "mode select waits, got {effects:?}");
    assert!(matches!(
        app.agents[&id].rewind_state.as_ref().unwrap().phase,
        crate::views::rewind::RewindPhase::ModeSelect {
            target_prompt_index: 1,
            ..
        }
    ));
}

/// Picking any target (including 0) opens confirm when the setting is on.
#[test]
fn picker_select_target_zero_opens_confirm() {
    let mut app = app_with_two_turns();
    let id = AgentId(0);

    dispatch(Action::RewindShowPicker, &mut app);
    dispatch(
        Action::TaskComplete(TaskResult::RewindPointsLoaded {
            agent_id: id,
            points: vec![rewind_point(1), rewind_point(0)],
        }),
        &mut app,
    );

    let effects = dispatch(Action::RewindPickerSelect(0), &mut app);
    assert!(effects.is_empty(), "mode select waits, got {effects:?}");
    assert!(matches!(
        app.agents[&id].rewind_state.as_ref().unwrap().phase,
        crate::views::rewind::RewindPhase::ModeSelect {
            target_prompt_index: 0,
            ..
        }
    ));
}

/// Confirm Yes executes conversation-only rewind.
#[test]
fn confirm_yes_executes_rewind() {
    let mut app = app_with_two_turns();
    let id = AgentId(0);

    dispatch(Action::RewindShowPicker, &mut app);
    dispatch(
        Action::TaskComplete(TaskResult::RewindPointsLoaded {
            agent_id: id,
            points: vec![rewind_point(1), rewind_point(0)],
        }),
        &mut app,
    );
    dispatch(Action::RewindPickerSelect(1), &mut app);
    dispatch(
        Action::RewindSelectMode {
            target: 1,
            mode: crate::views::rewind::RewindMode::ConversationOnly,
        },
        &mut app,
    );
    assert!(matches!(
        app.agents[&id].rewind_state.as_ref().unwrap().phase,
        crate::views::rewind::RewindPhase::Confirm {
            target_prompt_index: 1,
            ..
        }
    ));

    let effects = dispatch(Action::RewindConfirm(1), &mut app);
    assert!(
        matches!(
            &effects[0],
            Effect::RewindExecute {
                target_prompt_index: 1,
                ..
            }
        ),
        "got {effects:?}"
    );
    assert!(matches!(
        app.agents[&id].rewind_state.as_ref().unwrap().phase,
        crate::views::rewind::RewindPhase::Executing {
            target_prompt_index: 1,
            ..
        }
    ));
}

/// "Yes, and don't ask again" turns the setting off and executes this rewind.
#[test]
fn confirm_never_ask_persists_setting_off_and_executes() {
    let mut app = app_with_two_turns();
    assert!(app.current_ui.confirm_before_rewind_enabled());
    let id = AgentId(0);

    dispatch(Action::RewindShowPicker, &mut app);
    dispatch(
        Action::TaskComplete(TaskResult::RewindPointsLoaded {
            agent_id: id,
            points: vec![rewind_point(1), rewind_point(0)],
        }),
        &mut app,
    );
    dispatch(Action::RewindPickerSelect(1), &mut app);
    dispatch(
        Action::RewindSelectMode {
            target: 1,
            mode: crate::views::rewind::RewindMode::ConversationOnly,
        },
        &mut app,
    );

    let effects = dispatch(Action::RewindConfirmNeverAsk(1), &mut app);
    assert!(
        effects.iter().any(|e| matches!(
            e,
            Effect::PersistSetting {
                key: "confirm_before_rewind",
                value: crate::settings::SettingValue::Bool(false),
                ..
            }
        )),
        "must persist setting off, got {effects:?}"
    );
    assert!(
        effects.iter().any(|e| matches!(
            e,
            Effect::RewindExecute {
                target_prompt_index: 1,
                ..
            }
        )),
        "must execute rewind, got {effects:?}"
    );
    assert!(!app.current_ui.confirm_before_rewind_enabled());
    assert_eq!(app.current_ui.confirm_before_rewind, Some(false));
    assert!(
        app.agents[&id].toast.is_none(),
        "never-ask must not toast settings checkmark, got {:?}",
        app.agents[&id].toast
    );
    assert!(matches!(
        app.agents[&id].rewind_state.as_ref().unwrap().phase,
        crate::views::rewind::RewindPhase::Executing {
            target_prompt_index: 1,
            ..
        }
    ));
}

/// With confirm off, a conversation rewind to target 0 executes immediately (same as non-zero targets).
#[test]
fn picker_select_target_zero_executes_immediately_when_confirm_off() {
    let mut app = app_with_two_turns();
    app.current_ui.confirm_before_rewind = Some(false);
    let id = AgentId(0);

    dispatch(Action::RewindShowPicker, &mut app);
    dispatch(
        Action::TaskComplete(TaskResult::RewindPointsLoaded {
            agent_id: id,
            points: vec![rewind_point(1), rewind_point(0)],
        }),
        &mut app,
    );

    let effects = dispatch(Action::RewindPickerSelect(0), &mut app);
    assert!(effects.is_empty(), "mode select first, got {effects:?}");
    let effects = dispatch(
        Action::RewindSelectMode {
            target: 0,
            mode: crate::views::rewind::RewindMode::ConversationOnly,
        },
        &mut app,
    );
    assert!(
        matches!(
            &effects[0],
            Effect::RewindExecute {
                target_prompt_index: 0,
                mode: crate::views::rewind::RewindMode::ConversationOnly,
                ..
            }
        ),
        "got {effects:?}"
    );
    assert!(matches!(
        app.agents[&id].rewind_state.as_ref().unwrap().phase,
        crate::views::rewind::RewindPhase::Executing {
            target_prompt_index: 0,
            ..
        }
    ));
}

/// The files-only row is dim without tracked edits, so choosing it does nothing at all.
#[test]
fn files_only_without_tracked_edits_is_a_no_op() {
    let mut app = app_with_two_turns();
    app.current_ui.confirm_before_rewind = Some(false);
    let id = AgentId(0);

    dispatch(Action::RewindShowPicker, &mut app);
    dispatch(
        Action::TaskComplete(TaskResult::RewindPointsLoaded {
            agent_id: id,
            points: vec![rewind_point(1), rewind_point(0)],
        }),
        &mut app,
    );
    dispatch(Action::RewindPickerSelect(0), &mut app);

    let effects = dispatch(
        Action::RewindSelectMode {
            target: 0,
            mode: crate::views::rewind::RewindMode::FilesOnly,
        },
        &mut app,
    );
    assert!(effects.is_empty(), "nothing to restore, got {effects:?}");
    assert!(matches!(
        app.agents[&id].rewind_state.as_ref().unwrap().phase,
        crate::views::rewind::RewindPhase::ModeSelect {
            target_prompt_index: 0,
            has_file_changes: false,
            ..
        }
    ));
}

/// Non-zero success keeps earlier turns, truncates from the target, and toasts.
#[test]
fn rewind_success_nonzero_target_keeps_prefix_and_toasts() {
    let mut app = app_with_two_turns();
    let id = AgentId(0);
    let len_before = app.agents[&id].scrollback.len();

    dispatch(
        Action::TaskComplete(TaskResult::RewindExecuteComplete {
            agent_id: id,
            response: rewind_success(1, "turn 1"),
        }),
        &mut app,
    );

    let agent = &app.agents[&id];
    assert!(
        agent.scrollback.len() < len_before,
        "tail from target 1 must drop"
    );
    assert!(matches!(
        &agent.scrollback.entry(0).unwrap().block,
        RenderBlock::UserPrompt(b) if b.text == "turn 0"
    ));
    assert!(matches!(
        &agent.scrollback.entry(1).unwrap().block,
        RenderBlock::AgentMessage(_)
    ));
    assert_eq!(
        agent.scrollback.len(),
        2,
        "only turn 0 (prompt + reply) remains"
    );
    assert_eq!(
        agent.toast.as_ref().map(|(m, _)| m.as_str()),
        Some("Reverted conversation")
    );
}

/// Target-0 confirm then execute: the edited text is stashed exactly when the rewind executes.
/// On success the transcript truncates at the prompt and the edited text is resubmitted from there.
/// The editor closes and the composer draft survives (no "Reverted conversation" system note).
#[test]
fn inline_edit_conversation_only_success_resubmits_and_closes_editor() {
    let mut app = app_mid_inline_edit("fix the bug properly");
    let id = AgentId(0);
    dispatch(Action::InlineEditSubmit, &mut app);
    dispatch(points_loaded(id), &mut app);
    dispatch(
        Action::RewindSelectMode {
            target: 0,
            mode: crate::views::rewind::RewindMode::ConversationOnly,
        },
        &mut app,
    );
    assert!(matches!(
        app.agents[&id].rewind_state.as_ref().unwrap().phase,
        crate::views::rewind::RewindPhase::Confirm { .. }
    ));

    let effects = dispatch(Action::RewindConfirm(0), &mut app);
    assert!(matches!(
        &effects[0],
        Effect::RewindExecute {
            target_prompt_index: 0,
            ..
        }
    ));
    {
        let agent = &app.agents[&id];
        assert_eq!(
            agent.pending_inline_resubmit.as_deref(),
            Some("fix the bug properly"),
            "stash armed at execute time"
        );
        assert!(
            agent.inline_edit.is_some(),
            "editor open until the rewind lands"
        );
    }

    let effects = dispatch(
        Action::TaskComplete(TaskResult::RewindExecuteComplete {
            agent_id: id,
            response: rewind_success(0, "fix the bug"),
        }),
        &mut app,
    );

    assert!(
        effects.iter().any(
            |e| matches!(e, Effect::SendPrompt { text, .. } if text == "fix the bug properly")
        ),
        "edited prompt must be sent, got {effects:?}"
    );
    let agent = &app.agents[&id];
    assert!(agent.inline_edit.is_none(), "editor closed on success");
    assert!(agent.scrollback.inline_edit_height().is_none());
    assert!(agent.pending_inline_resubmit.is_none());
    assert_eq!(
        agent.prompt.text(),
        "composer draft",
        "composer draft survives"
    );
    // Transcript truncated at the prompt; only the resubmitted prompt block remains (no "Reverted conversation" system note)
    assert_eq!(agent.scrollback.len(), 1);
    match &agent.scrollback.entry(0).unwrap().block {
        RenderBlock::UserPrompt(b) => assert_eq!(b.text, "fix the bug properly"),
        other => panic!("expected resubmitted user prompt, got {other:?}"),
    }
}

/// A No or Esc dismiss from confirm during inline edit restores the editor draft.
#[test]
fn inline_edit_dismiss_from_confirm_keeps_editor() {
    let mut app = app_mid_inline_edit("fix the bug properly");
    let id = AgentId(0);
    dispatch(Action::InlineEditSubmit, &mut app);
    dispatch(points_loaded(id), &mut app);
    dispatch(
        Action::RewindSelectMode {
            target: 0,
            mode: crate::views::rewind::RewindMode::ConversationOnly,
        },
        &mut app,
    );
    assert!(matches!(
        app.agents[&id].rewind_state.as_ref().unwrap().phase,
        crate::views::rewind::RewindPhase::Confirm { .. }
    ));

    dispatch(Action::RewindDismiss, &mut app);

    let agent = &app.agents[&id];
    assert!(agent.rewind_state.is_none(), "overlay dismissed");
    assert_eq!(
        agent
            .inline_edit
            .as_ref()
            .expect("editor still open")
            .textarea
            .text(),
        "fix the bug properly"
    );
    assert!(agent.pending_inline_resubmit.is_none());
    assert_eq!(
        agent.prompt.text(),
        "composer draft",
        "composer draft restored on dismiss"
    );
}

/// Inline-edit of an older prompt with confirm off: the points load executes immediately with the resubmit stashed.
#[test]
fn inline_edit_nonzero_target_points_loaded_executes_immediately_when_confirm_off() {
    let mut app = test_app_with_agent();
    app.current_ui.confirm_before_rewind = Some(false);
    let id = AgentId(0);
    {
        let agent = app.agents.get_mut(&id).unwrap();
        agent.session.session_id = Some(acp::SessionId::new("sess".to_string()));
        for i in 0..2 {
            let mut b = UserPromptBlock::new(format!("turn {i}"));
            b.prompt_index = Some(i);
            agent.scrollback.push_block(RenderBlock::UserPrompt(b));
            agent
                .scrollback
                .push_block(RenderBlock::agent_message("ok"));
        }
        agent.scrollback.prepare_layout(80, 40);
        // Entry 2 is the second user prompt (index 1).
        assert!(agent.enter_inline_edit(2));
        agent
            .inline_edit
            .as_mut()
            .unwrap()
            .textarea
            .set_text("turn 1 edited");
        agent.prompt.set_text("composer draft");
    }

    let effects = dispatch(Action::InlineEditSubmit, &mut app);
    assert!(matches!(&effects[0], Effect::FetchRewindPoints { .. }));

    let effects = dispatch(
        Action::TaskComplete(TaskResult::RewindPointsLoaded {
            agent_id: id,
            points: vec![rewind_point(1), rewind_point(0)],
        }),
        &mut app,
    );
    assert!(effects.is_empty(), "mode question first, got {effects:?}");

    let effects = dispatch(
        Action::RewindSelectMode {
            target: 1,
            mode: crate::views::rewind::RewindMode::ConversationOnly,
        },
        &mut app,
    );
    assert!(
        matches!(
            &effects[0],
            Effect::RewindExecute {
                target_prompt_index: 1,
                ..
            }
        ),
        "got {effects:?}"
    );
    let agent = &app.agents[&id];
    assert!(matches!(
        agent.rewind_state.as_ref().unwrap().phase,
        crate::views::rewind::RewindPhase::Executing {
            target_prompt_index: 1,
            ..
        }
    ));
    assert_eq!(
        agent.pending_inline_resubmit.as_deref(),
        Some("turn 1 edited")
    );
    assert!(
        agent.inline_edit.is_some(),
        "editor open until execute lands"
    );
}

/// Inline-edit of an older prompt with confirm on (default): opens confirm.
#[test]
fn inline_edit_nonzero_target_opens_confirm_when_setting_on() {
    let mut app = test_app_with_agent();
    assert!(app.current_ui.confirm_before_rewind_enabled());
    let id = AgentId(0);
    {
        let agent = app.agents.get_mut(&id).unwrap();
        agent.session.session_id = Some(acp::SessionId::new("sess".to_string()));
        for i in 0..2 {
            let mut b = UserPromptBlock::new(format!("turn {i}"));
            b.prompt_index = Some(i);
            agent.scrollback.push_block(RenderBlock::UserPrompt(b));
            agent
                .scrollback
                .push_block(RenderBlock::agent_message("ok"));
        }
        agent.scrollback.prepare_layout(80, 40);
        assert!(agent.enter_inline_edit(2));
        agent
            .inline_edit
            .as_mut()
            .unwrap()
            .textarea
            .set_text("turn 1 edited");
    }

    dispatch(Action::InlineEditSubmit, &mut app);
    let effects = dispatch(
        Action::TaskComplete(TaskResult::RewindPointsLoaded {
            agent_id: id,
            points: vec![rewind_point(1), rewind_point(0)],
        }),
        &mut app,
    );
    assert!(effects.is_empty(), "mode question first, got {effects:?}");
    let effects = dispatch(
        Action::RewindSelectMode {
            target: 1,
            mode: crate::views::rewind::RewindMode::ConversationOnly,
        },
        &mut app,
    );
    assert!(effects.is_empty(), "confirm setting on, got {effects:?}");
    assert!(matches!(
        app.agents[&id].rewind_state.as_ref().unwrap().phase,
        crate::views::rewind::RewindPhase::Confirm {
            target_prompt_index: 1,
            active_idx: 0,
            ..
        }
    ));
    assert!(app.agents[&id].pending_inline_resubmit.is_none());
}

/// Points-loaded begin_rewind path: confirm off executes immediately.
#[test]
fn inline_edit_target_zero_executes_immediately_when_confirm_off() {
    let mut app = app_mid_inline_edit("fix the bug properly");
    app.current_ui.confirm_before_rewind = Some(false);
    let id = AgentId(0);

    dispatch(Action::InlineEditSubmit, &mut app);
    dispatch(points_loaded(id), &mut app);
    let effects = dispatch(
        Action::RewindSelectMode {
            target: 0,
            mode: crate::views::rewind::RewindMode::ConversationOnly,
        },
        &mut app,
    );
    assert!(
        matches!(
            &effects[0],
            Effect::RewindExecute {
                target_prompt_index: 0,
                ..
            }
        ),
        "got {effects:?}"
    );
    assert!(matches!(
        app.agents[&id].rewind_state.as_ref().unwrap().phase,
        crate::views::rewind::RewindPhase::Executing {
            target_prompt_index: 0,
            ..
        }
    ));
    assert_eq!(
        app.agents[&id].pending_inline_resubmit.as_deref(),
        Some("fix the bug properly")
    );
}

/// Classic points-loaded path with confirm off executes immediately.
#[test]
fn classic_points_loaded_target_zero_executes_when_confirm_off() {
    let mut app = test_app_with_agent();
    app.current_ui.confirm_before_rewind = Some(false);
    let id = AgentId(0);
    {
        let agent = app.agents.get_mut(&id).unwrap();
        agent
            .scrollback
            .push_block(RenderBlock::user_prompt("fix the bug"));
        agent
            .scrollback
            .push_block(RenderBlock::agent_message("done"));
        agent.scrollback.prepare_layout(80, 40);
        agent.scrollback.set_selected(Some(0));
    }

    dispatch(Action::Rewind, &mut app);
    dispatch(points_loaded(id), &mut app);
    let effects = dispatch(
        Action::RewindSelectMode {
            target: 0,
            mode: crate::views::rewind::RewindMode::ConversationOnly,
        },
        &mut app,
    );
    assert!(
        matches!(
            &effects[0],
            Effect::RewindExecute {
                target_prompt_index: 0,
                ..
            }
        ),
        "got {effects:?}"
    );
    assert!(matches!(
        app.agents[&id].rewind_state.as_ref().unwrap().phase,
        crate::views::rewind::RewindPhase::Executing {
            target_prompt_index: 0,
            ..
        }
    ));
}

/// Dismissing the mode question aborts: the overlay closes, nothing was stashed, and the editor is still open with the edit intact.
#[test]
fn inline_edit_dismiss_from_mode_select_returns_to_editor() {
    let mut app = app_mid_inline_edit("fix the bug properly");
    let id = AgentId(0);
    dispatch(Action::InlineEditSubmit, &mut app);
    dispatch(points_loaded(id), &mut app);

    dispatch(Action::RewindDismiss, &mut app);

    let agent = &app.agents[&id];
    assert!(agent.rewind_state.is_none());
    assert!(agent.pending_inline_resubmit.is_none());
    assert_eq!(
        agent
            .inline_edit
            .as_ref()
            .expect("editor still open")
            .textarea
            .text(),
        "fix the bug properly"
    );
}

/// Submitting mid-turn raises the same cancel-offer `/rewind` does, pre-targeted at the edited prompt, over the still-open editor.
/// Confirming cancels the turn and re-enters the flow via a points fetch.
#[test]
fn inline_edit_busy_submit_cancel_offer_confirm_cancels_and_fetches_points() {
    let mut app = app_mid_inline_edit("fix the bug properly");
    let id = AgentId(0);
    app.agents.get_mut(&id).unwrap().session.state = crate::app::agent::AgentState::TurnRunning;

    let effects = dispatch(Action::InlineEditSubmit, &mut app);
    assert!(effects.is_empty(), "no effects yet: {effects:?}");
    {
        let agent = &app.agents[&id];
        let state = agent.rewind_state.as_ref().unwrap();
        assert!(matches!(
            state.phase,
            crate::views::rewind::RewindPhase::CancelOffer { .. }
        ));
        assert_eq!(state.selected_prompt_index, Some(0));
        assert!(agent.inline_edit.is_some(), "editor open behind the offer");
        assert!(agent.pending_inline_resubmit.is_none());
    }

    let effects = dispatch(Action::RewindCancelOffer, &mut app);
    assert!(matches!(&effects[0], Effect::CancelTurn { .. }));
    assert!(
        effects
            .iter()
            .any(|e| matches!(e, Effect::FetchRewindPoints { .. })),
        "got {effects:?}"
    );
    let agent = &app.agents[&id];
    let state = agent.rewind_state.as_ref().unwrap();
    assert!(matches!(
        state.phase,
        crate::views::rewind::RewindPhase::Loading
    ));
    assert_eq!(
        state.selected_prompt_index,
        Some(0),
        "target survives the cancel"
    );
    assert!(agent.inline_edit.is_some(), "editor still open");
}

/// Dismissing the mid-turn cancel-offer ("let it finish") returns straight to the still-open editor with the edit intact.
#[test]
fn inline_edit_busy_cancel_offer_dismiss_returns_to_editor() {
    let mut app = app_mid_inline_edit("fix the bug properly");
    let id = AgentId(0);
    app.agents.get_mut(&id).unwrap().session.state = crate::app::agent::AgentState::TurnRunning;
    dispatch(Action::InlineEditSubmit, &mut app);

    dispatch(Action::RewindDismiss, &mut app);

    let agent = &app.agents[&id];
    assert!(agent.rewind_state.is_none());
    assert_eq!(
        agent
            .inline_edit
            .as_ref()
            .expect("editor open")
            .textarea
            .text(),
        "fix the bug properly"
    );
}

/// A failed execute drops the stashed resubmit but leaves the editor open behind the error overlay.
/// Dismissing the error returns to editing.
#[test]
fn inline_edit_execute_failure_keeps_editor_open() {
    let mut app = app_mid_inline_edit("fix the bug properly");
    let id = AgentId(0);
    drive_inline_submit_to_execute(&mut app);
    assert!(app.agents[&id].pending_inline_resubmit.is_some());

    let effects = dispatch(
        Action::TaskComplete(TaskResult::RewindExecuteFailed {
            agent_id: id,
            error: "boom".into(),
        }),
        &mut app,
    );

    assert!(effects.is_empty());
    let agent = &app.agents[&id];
    assert!(
        agent.pending_inline_resubmit.is_none(),
        "stash dies with its rewind"
    );
    match &agent.rewind_state.as_ref().unwrap().phase {
        crate::views::rewind::RewindPhase::Error { message } => {
            assert_eq!(message, "boom");
        }
        other => panic!("expected Error, got {other:?}"),
    }
    assert_eq!(
        agent
            .inline_edit
            .as_ref()
            .expect("editor open")
            .textarea
            .text(),
        "fix the bug properly"
    );
    assert_eq!(agent.scrollback.len(), 2, "transcript untouched");
}

/// A rewind/execute response with `success: false` likewise drops the stash, shows the error overlay, and leaves the editor open.
#[test]
fn inline_edit_unsuccessful_response_keeps_editor_open() {
    let mut app = app_mid_inline_edit("fix the bug properly");
    let id = AgentId(0);
    drive_inline_submit_to_execute(&mut app);

    let mut response = rewind_success(0, "fix the bug");
    response.success = false;
    response.error = Some("conflict".into());
    let effects = dispatch(
        Action::TaskComplete(TaskResult::RewindExecuteComplete {
            agent_id: id,
            response,
        }),
        &mut app,
    );

    assert!(effects.is_empty());
    let agent = &app.agents[&id];
    assert!(agent.pending_inline_resubmit.is_none());
    match &agent.rewind_state.as_ref().unwrap().phase {
        crate::views::rewind::RewindPhase::Error { message } => {
            assert_eq!(message, "conflict");
        }
        other => panic!("expected Error, got {other:?}"),
    }
    assert!(agent.inline_edit.is_some(), "editor stays open");
    assert_eq!(agent.scrollback.len(), 2, "transcript untouched");
}

/// An edited prompt that happens to start with "/" is resubmitted verbatim as a prompt.
/// It must not run as a slash command on the already-truncated transcript.
#[test]
fn inline_edit_resubmit_sends_slash_text_literally() {
    let mut app = app_mid_inline_edit("/etc/hosts is wrong, fix it");
    let id = AgentId(0);
    drive_inline_submit_to_execute(&mut app);

    let effects = dispatch(
        Action::TaskComplete(TaskResult::RewindExecuteComplete {
            agent_id: id,
            response: rewind_success(0, "fix the bug"),
        }),
        &mut app,
    );

    assert!(
        effects.iter().any(
            |e| matches!(e, Effect::SendPrompt { text, .. } if text == "/etc/hosts is wrong, fix it")
        ),
        "slash-lookalike edit must be sent as a prompt, got {effects:?}"
    );
}

/// If the user switches views while the rewind is in flight, the edited text falls back into that agent's composer.
/// It is appended on a new line rather than overwriting an existing draft.
#[test]
fn inline_edit_rewind_success_after_view_switch_appends_to_draft() {
    let mut app = app_mid_inline_edit("fix the bug properly");
    let id = AgentId(0);
    drive_inline_submit_to_execute(&mut app);
    app.active_view = ActiveView::Welcome;

    let effects = dispatch(
        Action::TaskComplete(TaskResult::RewindExecuteComplete {
            agent_id: id,
            response: rewind_success(0, "fix the bug"),
        }),
        &mut app,
    );

    assert!(
        effects
            .iter()
            .all(|e| !matches!(e, Effect::SendPrompt { .. })),
        "no resubmit while the view is elsewhere, got {effects:?}"
    );
    let agent = &app.agents[&id];
    assert_eq!(
        agent.prompt.text(),
        "composer draft\nfix the bug properly",
        "draft preserved, edited text appended"
    );
    assert!(agent.inline_edit.is_none(), "editor closed on success");
}

#[test]
fn inline_edit_view_switch_preserves_image_draft_when_appending_resubmit() {
    let mut app = app_mid_inline_edit("fix the bug properly");
    let id = AgentId(0);
    let draft_text = {
        let agent = app.agents.get_mut(&id).unwrap();
        let end = agent.prompt.text().len();
        agent.prompt.set_cursor(end);
        agent
            .prompt
            .insert_image(crate::prompt_images::from_clipboard_data(
                &crate::clipboard::ImageData {
                    data: vec![1, 2, 3],
                    mime_type: "image/png".into(),
                },
            ))
            .unwrap();
        agent.prompt.text().to_owned()
    };
    drive_inline_submit_to_execute(&mut app);
    app.active_view = ActiveView::Welcome;

    let effects = dispatch(
        Action::TaskComplete(TaskResult::RewindExecuteComplete {
            agent_id: id,
            response: rewind_success(0, "fix the bug"),
        }),
        &mut app,
    );

    assert!(
        effects
            .iter()
            .all(|effect| !matches!(effect, Effect::SendPrompt { .. }))
    );
    let agent = app.agents.get_mut(&id).unwrap();
    assert_eq!(
        agent.prompt.text(),
        format!("{draft_text}\nfix the bug properly")
    );
    let restored_image_ids = agent
        .prompt
        .textarea
        .elements()
        .iter()
        .filter(|element| element.kind == crate::views::prompt_widget::KIND_IMAGE)
        .map(|element| element.id)
        .collect::<Vec<_>>();
    assert_eq!(
        restored_image_ids.len(),
        1,
        "restored draft must retain one image chip",
    );
    let images = agent.prompt.drain_images();
    assert_eq!(images.len(), 1, "reconciliation must retain the image");
    assert_eq!(
        images[0].element_id, restored_image_ids[0],
        "restored image must bind to the re-registered chip element",
    );
}

#[test]
fn stacked_rewinds_each_get_their_own_pid_and_orphans_drop_independently() {
    // Two rewinds leave two cancelled PromptResponses to drain
    // Each carries its own promptId; both fail to match current_prompt_id (None) and are silently discarded with no banner
    let mut app = test_app_with_agent();
    let id = AgentId(0);

    dispatch(Action::SendPrompt("a".into()), &mut app);
    let pid_a = app.agents[&id].session.current_prompt_id.clone();
    dispatch(Action::CancelTurn, &mut app);
    dispatch(Action::SendPrompt("b".into()), &mut app);
    let pid_b = app.agents[&id].session.current_prompt_id.clone();
    dispatch(Action::CancelTurn, &mut app);
    assert_ne!(pid_a, pid_b);
    assert!(app.agents[&id].session.current_prompt_id.is_none());

    let pr = |pid: &Option<String>| {
        Action::TaskComplete(TaskResult::PromptResponse {
            agent_id: id,
            result: Ok(acp::PromptResponse::new(acp::StopReason::Cancelled)
                .meta(serde_json::json!({ "promptId": pid }).as_object().cloned())),
            http_status: None,
            prompt_id: None,
        })
    };
    dispatch(pr(&pid_a), &mut app);
    dispatch(pr(&pid_b), &mut app);
    assert_eq!(app.agents[&id].scrollback.len(), 0);
    assert!(app.agents[&id].session.state.is_idle());
}

fn user_block(text: &str, pi: Option<usize>) -> RenderBlock {
    let mut b = UserPromptBlock::new(text);
    b.prompt_index = pi;
    RenderBlock::UserPrompt(b)
}

/// A successful conversation rewind truncates the transcript tail (`remove_from`); the purge must fire exactly once.
#[test]
fn rewind_success_truncation_releases_retained_memory() {
    use crate::memory_release::test_support;
    test_support::install_counting_hook();

    let response = crate::views::rewind::RewindResponse {
        success: true,
        target_prompt_index: 0,
        reverted_files: Vec::new(),
        clean_files: Vec::new(),
        conflicts: Vec::new(),
        error: None,
        mode: Some("conversation_only".into()),
        prompt_text: Some("alpha".into()),
    };

    let mut app = test_app_with_agent();
    let id = AgentId(0);
    if let Some(agent) = app.agents.get_mut(&id) {
        agent.scrollback.push_block(user_block("alpha", Some(0)));
        agent.scrollback.push_block(RenderBlock::agent_message("a"));
    }
    let len_before = app.agents[&id].scrollback.len();
    let before = test_support::calls();
    dispatch(
        Action::TaskComplete(TaskResult::RewindExecuteComplete {
            agent_id: id,
            response,
        }),
        &mut app,
    );
    assert!(
        app.agents[&id].scrollback.len() < len_before,
        "fixture sanity: the conversation rewind must truncate entries"
    );
    assert_eq!(
        test_support::calls(),
        before + 1,
        "the rewound tail dropped — exactly one purge"
    );
}

/// A successful rewind confirms via a toast in the full TUI; minimal mode keeps the scrollback system block (it never renders toasts).
#[test]
fn rewind_success_toasts_in_full_tui_and_commits_system_block_in_minimal() {
    let response = crate::views::rewind::RewindResponse {
        success: true,
        target_prompt_index: 0,
        reverted_files: Vec::new(),
        clean_files: Vec::new(),
        conflicts: Vec::new(),
        error: None,
        mode: Some("conversation_only".into()),
        prompt_text: None,
    };

    let mut app = test_app_with_agent();
    let id = AgentId(0);
    if let Some(agent) = app.agents.get_mut(&id) {
        agent.scrollback.push_block(user_block("alpha", Some(0)));
        agent.scrollback.push_block(RenderBlock::agent_message("a"));
    }
    dispatch(
        Action::TaskComplete(TaskResult::RewindExecuteComplete {
            agent_id: id,
            response: response.clone(),
        }),
        &mut app,
    );
    assert_eq!(
        app.agents[&id].toast.as_ref().map(|(m, _)| m.as_str()),
        Some("Reverted conversation")
    );
    assert_eq!(
        app.agents[&id].scrollback.len(),
        0,
        "the confirmation must not land in scrollback in the full TUI"
    );

    let mut app = test_app_with_agent();
    app.screen_mode = crate::app::ScreenMode::Minimal;
    if let Some(agent) = app.agents.get_mut(&id) {
        agent.scrollback.push_block(user_block("alpha", Some(0)));
        agent.scrollback.push_block(RenderBlock::agent_message("a"));
    }
    dispatch(
        Action::TaskComplete(TaskResult::RewindExecuteComplete {
            agent_id: id,
            response,
        }),
        &mut app,
    );
    assert!(app.agents[&id].toast.is_none());
    assert_eq!(last_system_text(&app, id), "Reverted conversation");
}

#[test]
fn primary_path_returns_correct_idx_for_each_prompt() {
    let mut sb = ScrollbackState::new();
    let alpha = sb.push_block(user_block("alpha", Some(0)));
    sb.push_block(RenderBlock::agent_message("a"));
    let bravo = sb.push_block(user_block("bravo", Some(1)));
    sb.push_block(RenderBlock::agent_message("b"));
    let charlie = sb.push_block(user_block("charlie", Some(2)));
    sb.push_block(RenderBlock::agent_message("c"));

    let alpha_idx = sb.index_of_id(alpha).unwrap();
    let bravo_idx = sb.index_of_id(bravo).unwrap();
    let charlie_idx = sb.index_of_id(charlie).unwrap();

    assert_eq!(
        find_user_prompt_entry_for_shell_index(&sb, 0),
        Some(alpha_idx)
    );
    assert_eq!(
        find_user_prompt_entry_for_shell_index(&sb, 1),
        Some(bravo_idx)
    );
    assert_eq!(
        find_user_prompt_entry_for_shell_index(&sb, 2),
        Some(charlie_idx)
    );
}

/// Interjections render as standard user prompts but the shell never numbers them.
/// The positional fallback must skip them or every mapping after an interjection is off by one.
#[test]
fn fallback_path_skips_interjections() {
    let mut sb = ScrollbackState::new();
    let alpha = sb.push_block(user_block("alpha", None));
    sb.push_block(RenderBlock::agent_message("a"));
    sb.push_block(RenderBlock::interjection_prompt("mid-turn steer"));
    sb.push_block(RenderBlock::agent_message("a2"));
    let bravo = sb.push_block(user_block("bravo", None));

    let alpha_idx = sb.index_of_id(alpha).unwrap();
    let bravo_idx = sb.index_of_id(bravo).unwrap();

    assert_eq!(
        find_user_prompt_entry_for_shell_index(&sb, 0),
        Some(alpha_idx)
    );
    assert_eq!(
        find_user_prompt_entry_for_shell_index(&sb, 1),
        Some(bravo_idx),
        "index 1 must map to the next real prompt, not the interjection"
    );
}

/// Selecting an interjection (or an entry after it within the same turn) anchors rewind on the enclosing turn's prompt, not the next turn's.
#[test]
fn shell_prompt_index_at_resolves_interjection_to_enclosing_turn() {
    use super::super::rewind::shell_prompt_index_at;

    let mut sb = ScrollbackState::new();
    sb.push_block(user_block("alpha", Some(0)));
    sb.push_block(RenderBlock::agent_message("a"));
    let ij = sb.push_block(RenderBlock::interjection_prompt("mid-turn steer"));
    sb.push_block(RenderBlock::agent_message("a2"));
    sb.push_block(user_block("bravo", Some(1)));

    let ij_idx = sb.index_of_id(ij).unwrap();
    assert_eq!(shell_prompt_index_at(&sb, ij_idx), Some(0));
    // A block after the interjection but before the next prompt still belongs to turn 0
    assert_eq!(shell_prompt_index_at(&sb, ij_idx + 1), Some(0));
}

/// Legacy meta-less scrollbacks: the positional count inside `shell_prompt_index_at` must also exclude interjections.
#[test]
fn shell_prompt_index_at_counting_fallback_skips_interjections() {
    use super::super::rewind::shell_prompt_index_at;

    let mut sb = ScrollbackState::new();
    sb.push_block(user_block("alpha", None));
    sb.push_block(RenderBlock::interjection_prompt("steer"));
    let bravo = sb.push_block(user_block("bravo", None));

    let bravo_idx = sb.index_of_id(bravo).unwrap();
    assert_eq!(shell_prompt_index_at(&sb, bravo_idx), Some(1));
}

#[test]
fn fallback_path_returns_correct_idx_when_prompt_index_is_none() {
    let mut sb = ScrollbackState::new();
    let alpha = sb.push_block(user_block("alpha", None));
    sb.push_block(RenderBlock::agent_message("a"));
    let bravo = sb.push_block(user_block("bravo", None));
    sb.push_block(RenderBlock::agent_message("b"));
    let charlie = sb.push_block(user_block("charlie", None));
    sb.push_block(RenderBlock::agent_message("c"));

    let alpha_idx = sb.index_of_id(alpha).unwrap();
    let bravo_idx = sb.index_of_id(bravo).unwrap();
    let charlie_idx = sb.index_of_id(charlie).unwrap();

    assert_eq!(
        find_user_prompt_entry_for_shell_index(&sb, 0),
        Some(alpha_idx)
    );
    assert_eq!(
        find_user_prompt_entry_for_shell_index(&sb, 1),
        Some(bravo_idx)
    );
    assert_eq!(
        find_user_prompt_entry_for_shell_index(&sb, 2),
        Some(charlie_idx)
    );
}

/// Two points, the later one carrying tracked edits: any restore to turn 1 moves files.
fn app_with_tracked_edits() -> AppView {
    let mut app = app_with_two_turns();
    let id = AgentId(0);
    dispatch(Action::RewindShowPicker, &mut app);
    dispatch(
        points_loaded_with(id, vec![rewind_point_with_files(1, 2), rewind_point(0)]),
        &mut app,
    );
    app
}

/// Choosing "both" on a turn with tracked edits previews first: no write goes out yet.
#[test]
fn select_all_with_tracked_edits_previews_before_writing() {
    let mut app = app_with_tracked_edits();
    let id = AgentId(0);
    dispatch(Action::RewindPickerSelect(1), &mut app);
    assert!(matches!(
        app.agents[&id].rewind_state.as_ref().unwrap().phase,
        crate::views::rewind::RewindPhase::ModeSelect {
            has_file_changes: true,
            ..
        }
    ));

    let effects = dispatch(
        Action::RewindSelectMode {
            target: 1,
            mode: crate::views::rewind::RewindMode::All,
        },
        &mut app,
    );
    assert!(
        matches!(
            effects.as_slice(),
            [Effect::RewindPreview {
                target_prompt_index: 1,
                mode: crate::views::rewind::RewindMode::All,
                ..
            }]
        ),
        "got {effects:?}"
    );
    assert!(
        !effects
            .iter()
            .any(|e| matches!(e, Effect::RewindExecute { .. })),
        "nothing may be written before the confirm"
    );
    assert!(matches!(
        app.agents[&id].rewind_state.as_ref().unwrap().phase,
        crate::views::rewind::RewindPhase::Previewing {
            target_prompt_index: 1,
            mode: crate::views::rewind::RewindMode::All
        }
    ));
}

/// The dry run's clean paths and conflicts become the confirm list; Yes then commits.
#[test]
fn preview_complete_lists_files_and_confirm_executes() {
    let mut app = app_with_tracked_edits();
    let id = AgentId(0);
    dispatch(Action::RewindPickerSelect(1), &mut app);
    dispatch(
        Action::RewindSelectMode {
            target: 1,
            mode: crate::views::rewind::RewindMode::All,
        },
        &mut app,
    );

    dispatch(
        preview_complete(
            id,
            1,
            crate::views::rewind::RewindMode::All,
            preview_response(1, &["src/a.rs"], &[("src/b.rs", "modified_externally")]),
        ),
        &mut app,
    );

    match &app.agents[&id].rewind_state.as_ref().unwrap().phase {
        crate::views::rewind::RewindPhase::FilePreview {
            clean_files,
            conflicts,
            mode,
            ..
        } => {
            assert_eq!(clean_files, &["src/a.rs".to_string()]);
            assert_eq!(conflicts.len(), 1);
            assert_eq!(conflicts[0].path, "src/b.rs");
            assert_eq!(conflicts[0].label, "modified");
            assert_eq!(*mode, crate::views::rewind::RewindMode::All);
        }
        other => panic!("expected FilePreview, got {other:?}"),
    }

    let effects = dispatch(Action::RewindConfirm(1), &mut app);
    assert!(
        matches!(
            effects.as_slice(),
            [Effect::RewindExecute {
                target_prompt_index: 1,
                mode: crate::views::rewind::RewindMode::All,
                ..
            }]
        ),
        "got {effects:?}"
    );
}

/// A preview with no conflicts still comes back `success: false` from the engine.
/// That is the normal dry-run shape, not a failure.
#[test]
fn preview_with_zero_conflicts_lands_on_file_preview() {
    let mut app = app_with_tracked_edits();
    let id = AgentId(0);
    dispatch(Action::RewindPickerSelect(1), &mut app);
    dispatch(
        Action::RewindSelectMode {
            target: 1,
            mode: crate::views::rewind::RewindMode::FilesOnly,
        },
        &mut app,
    );

    let response = preview_response(1, &["src/a.rs"], &[]);
    assert!(!response.success);
    assert!(response.error.is_none());
    dispatch(
        preview_complete(id, 1, crate::views::rewind::RewindMode::FilesOnly, response),
        &mut app,
    );

    match &app.agents[&id].rewind_state.as_ref().unwrap().phase {
        crate::views::rewind::RewindPhase::FilePreview {
            clean_files,
            conflicts,
            ..
        } => {
            assert_eq!(clean_files, &["src/a.rs".to_string()]);
            assert!(conflicts.is_empty());
        }
        other => panic!("expected FilePreview, got {other:?}"),
    }
}

/// A real preview failure (an invalid target, say) comes back with both lists empty.
#[test]
fn preview_error_with_empty_lists_shows_the_error() {
    let mut app = app_with_tracked_edits();
    let id = AgentId(0);
    dispatch(Action::RewindPickerSelect(1), &mut app);
    dispatch(
        Action::RewindSelectMode {
            target: 1,
            mode: crate::views::rewind::RewindMode::All,
        },
        &mut app,
    );

    let mut response = preview_response(1, &[], &[]);
    response.error = Some("invalid target".into());
    dispatch(
        preview_complete(id, 1, crate::views::rewind::RewindMode::All, response),
        &mut app,
    );

    match &app.agents[&id].rewind_state.as_ref().unwrap().phase {
        crate::views::rewind::RewindPhase::Error { message } => {
            assert_eq!(message, "invalid target");
        }
        other => panic!("expected Error, got {other:?}"),
    }
}

/// Esc during the dry run ends the flow; the late result must not resurrect it.
#[test]
fn late_preview_result_after_dismiss_is_ignored() {
    let mut app = app_with_tracked_edits();
    let id = AgentId(0);
    dispatch(Action::RewindPickerSelect(1), &mut app);
    dispatch(
        Action::RewindSelectMode {
            target: 1,
            mode: crate::views::rewind::RewindMode::All,
        },
        &mut app,
    );
    dispatch(Action::RewindDismiss, &mut app);
    assert!(app.agents[&id].rewind_state.is_none());

    let effects = dispatch(
        preview_complete(
            id,
            1,
            crate::views::rewind::RewindMode::All,
            preview_response(1, &["src/a.rs"], &[]),
        ),
        &mut app,
    );
    assert!(effects.is_empty(), "got {effects:?}");
    assert!(
        app.agents[&id].rewind_state.is_none(),
        "a stale preview must not reopen the overlay"
    );
}

/// Backspace from the file list returns to the mode question with the facts re-derived.
#[test]
fn back_from_file_preview_returns_to_mode_select() {
    let mut app = app_with_tracked_edits();
    let id = AgentId(0);
    dispatch(Action::RewindPickerSelect(1), &mut app);
    dispatch(
        Action::RewindSelectMode {
            target: 1,
            mode: crate::views::rewind::RewindMode::All,
        },
        &mut app,
    );
    dispatch(
        preview_complete(
            id,
            1,
            crate::views::rewind::RewindMode::All,
            preview_response(1, &["src/a.rs"], &[]),
        ),
        &mut app,
    );

    let effects = dispatch(Action::RewindBackToModeSelect, &mut app);
    assert!(effects.is_empty(), "got {effects:?}");
    assert!(matches!(
        app.agents[&id].rewind_state.as_ref().unwrap().phase,
        crate::views::rewind::RewindPhase::ModeSelect {
            target_prompt_index: 1,
            has_file_changes: true,
            offer_files_only: true,
            active_idx: 0,
            ..
        }
    ));
}

/// A files-only rewind leaves the transcript and the composer draft exactly as they were.
#[test]
fn files_only_success_keeps_transcript_and_draft() {
    let mut app = app_with_two_turns();
    let id = AgentId(0);
    app.agents
        .get_mut(&id)
        .unwrap()
        .prompt
        .set_text("composer draft");

    dispatch(Action::RewindShowPicker, &mut app);
    dispatch(
        points_loaded_with(id, vec![rewind_point_with_files(1, 2), rewind_point(0)]),
        &mut app,
    );
    dispatch(Action::RewindPickerSelect(1), &mut app);
    dispatch(
        Action::RewindSelectMode {
            target: 1,
            mode: crate::views::rewind::RewindMode::FilesOnly,
        },
        &mut app,
    );
    dispatch(
        preview_complete(
            id,
            1,
            crate::views::rewind::RewindMode::FilesOnly,
            preview_response(1, &["src/a.rs", "src/b.rs"], &[]),
        ),
        &mut app,
    );
    let effects = dispatch(Action::RewindConfirm(1), &mut app);
    assert!(
        matches!(
            effects.as_slice(),
            [Effect::RewindExecute {
                mode: crate::views::rewind::RewindMode::FilesOnly,
                ..
            }]
        ),
        "got {effects:?}"
    );

    let len_before = app.agents[&id].scrollback.len();
    let pane_before = app.agents[&id].active_pane;
    dispatch(
        Action::TaskComplete(TaskResult::RewindExecuteComplete {
            agent_id: id,
            response: crate::views::rewind::RewindResponse {
                success: true,
                target_prompt_index: 1,
                reverted_files: vec!["src/a.rs".into(), "src/b.rs".into()],
                clean_files: vec![],
                conflicts: vec![],
                error: None,
                mode: Some("files_only".into()),
                prompt_text: Some("turn 1".into()),
            },
        }),
        &mut app,
    );

    let agent = &app.agents[&id];
    assert_eq!(
        agent.scrollback.len(),
        len_before,
        "a files-only rewind must not truncate the transcript"
    );
    assert_eq!(
        agent.prompt.text(),
        "composer draft",
        "the draft comes back untouched; prompt_text is ignored"
    );
    assert_eq!(
        agent.toast.as_ref().map(|(m, _)| m.as_str()),
        Some("Reverted 2 files")
    );
    assert_eq!(
        agent.active_pane, pane_before,
        "a files-only rewind must not steal focus back to the composer"
    );
}

/// A conversation rewind to prompt 0 wipes the snapshots, so the file changes it strands
/// can never be undone. That gets its own warning before anything happens.
#[test]
fn conversation_only_at_zero_with_tracked_edits_warns_about_orphans() {
    let mut app = app_with_two_turns();
    let id = AgentId(0);
    dispatch(Action::RewindShowPicker, &mut app);
    dispatch(
        points_loaded_with(id, vec![rewind_point_with_files(1, 2), rewind_point(0)]),
        &mut app,
    );
    dispatch(Action::RewindPickerSelect(0), &mut app);

    let effects = dispatch(
        Action::RewindSelectMode {
            target: 0,
            mode: crate::views::rewind::RewindMode::ConversationOnly,
        },
        &mut app,
    );
    assert!(effects.is_empty(), "the warning waits, got {effects:?}");
    assert!(matches!(
        app.agents[&id].rewind_state.as_ref().unwrap().phase,
        crate::views::rewind::RewindPhase::OrphanWarning {
            target_prompt_index: 0,
            ..
        }
    ));

    let effects = dispatch(Action::RewindConfirm(0), &mut app);
    assert!(
        matches!(
            effects.as_slice(),
            [Effect::RewindExecute {
                target_prompt_index: 0,
                mode: crate::views::rewind::RewindMode::ConversationOnly,
                ..
            }]
        ),
        "got {effects:?}"
    );
}

/// Without tracked edits there is nothing to orphan, so prompt 0 takes the standard confirm.
#[test]
fn conversation_only_at_zero_without_tracked_edits_takes_the_confirm() {
    let mut app = app_with_two_turns();
    let id = AgentId(0);
    assert!(app.current_ui.confirm_before_rewind_enabled());
    dispatch(Action::RewindShowPicker, &mut app);
    dispatch(
        points_loaded_with(id, vec![rewind_point(1), rewind_point(0)]),
        &mut app,
    );
    dispatch(Action::RewindPickerSelect(0), &mut app);

    dispatch(
        Action::RewindSelectMode {
            target: 0,
            mode: crate::views::rewind::RewindMode::ConversationOnly,
        },
        &mut app,
    );
    assert!(matches!(
        app.agents[&id].rewind_state.as_ref().unwrap().phase,
        crate::views::rewind::RewindPhase::Confirm {
            target_prompt_index: 0,
            mode: crate::views::rewind::RewindMode::ConversationOnly,
            ..
        }
    ));
}

/// Inline edit-and-resubmit over a turn with tracked edits: the files-only row is hidden,
/// "both" still previews, and the resubmit fires once the rewind lands.
#[test]
fn inline_edit_all_with_tracked_edits_previews_then_resubmits() {
    let mut app = app_mid_inline_edit("fix the bug properly");
    let id = AgentId(0);
    dispatch(Action::InlineEditSubmit, &mut app);
    dispatch(
        points_loaded_with(id, vec![rewind_point_with_files(0, 3)]),
        &mut app,
    );
    assert!(matches!(
        app.agents[&id].rewind_state.as_ref().unwrap().phase,
        crate::views::rewind::RewindPhase::ModeSelect {
            has_file_changes: true,
            offer_files_only: false,
            ..
        }
    ));

    let effects = dispatch(
        Action::RewindSelectMode {
            target: 0,
            mode: crate::views::rewind::RewindMode::All,
        },
        &mut app,
    );
    assert!(
        matches!(effects.as_slice(), [Effect::RewindPreview { .. }]),
        "got {effects:?}"
    );

    dispatch(
        preview_complete(
            id,
            0,
            crate::views::rewind::RewindMode::All,
            preview_response(0, &["src/a.rs"], &[]),
        ),
        &mut app,
    );
    assert!(matches!(
        app.agents[&id].rewind_state.as_ref().unwrap().phase,
        crate::views::rewind::RewindPhase::FilePreview { .. }
    ));

    let effects = dispatch(Action::RewindConfirm(0), &mut app);
    assert!(
        matches!(
            effects.as_slice(),
            [Effect::RewindExecute {
                mode: crate::views::rewind::RewindMode::All,
                ..
            }]
        ),
        "got {effects:?}"
    );

    let effects = dispatch(
        Action::TaskComplete(TaskResult::RewindExecuteComplete {
            agent_id: id,
            response: crate::views::rewind::RewindResponse {
                success: true,
                target_prompt_index: 0,
                reverted_files: vec!["src/a.rs".into()],
                clean_files: vec![],
                conflicts: vec![],
                error: None,
                mode: Some("all".into()),
                prompt_text: Some("fix the bug".into()),
            },
        }),
        &mut app,
    );
    assert!(
        effects.iter().any(
            |e| matches!(e, Effect::SendPrompt { text, .. } if text == "fix the bug properly")
        ),
        "edited prompt must be sent, got {effects:?}"
    );
    let agent = &app.agents[&id];
    assert!(agent.inline_edit.is_none(), "editor closed on success");
    assert_eq!(
        agent.toast.as_ref().map(|(m, _)| m.as_str()),
        Some("Reverted 1 file"),
        "the file revert still needs a signal behind the resubmit"
    );
}

/// A restore to N reverts every path touched at prompt index >= N, so the question is
/// cumulative over the later points rather than per point.
#[test]
fn has_tracked_edits_from_is_cumulative() {
    use super::super::rewind::has_tracked_edits_from;

    let points = vec![
        rewind_point_with_files(1, 0),
        rewind_point_with_files(2, 3),
        rewind_point_with_files(3, 0),
    ];
    assert!(
        has_tracked_edits_from(&points, 1),
        "turn 2's edits are inside a restore to turn 1"
    );
    assert!(has_tracked_edits_from(&points, 2));
    assert!(
        !has_tracked_edits_from(&points, 3),
        "nothing tracked at or after turn 3"
    );
}

/// A dry run abandoned with Esc can still time out later. Its failure carries the target and
/// mode it was launched with, so it cannot replace whatever flow is on screen by then.
#[test]
fn stale_preview_failure_cannot_clobber_a_newer_flow() {
    let mut app = app_with_tracked_edits();
    let id = AgentId(0);

    // Start a preview for turn 1, abandon it, then start a fresh one for turn 0.
    dispatch(Action::RewindPickerSelect(1), &mut app);
    dispatch(
        Action::RewindSelectMode {
            target: 1,
            mode: crate::views::rewind::RewindMode::All,
        },
        &mut app,
    );
    dispatch(Action::RewindDismiss, &mut app);
    dispatch(Action::RewindShowPicker, &mut app);
    dispatch(
        points_loaded_with(id, vec![rewind_point_with_files(1, 2), rewind_point(0)]),
        &mut app,
    );
    dispatch(Action::RewindPickerSelect(0), &mut app);
    dispatch(
        Action::RewindSelectMode {
            target: 0,
            mode: crate::views::rewind::RewindMode::All,
        },
        &mut app,
    );
    assert!(matches!(
        app.agents[&id].rewind_state.as_ref().unwrap().phase,
        crate::views::rewind::RewindPhase::Previewing {
            target_prompt_index: 0,
            ..
        }
    ));

    // The abandoned turn-1 request finally errors out.
    let effects = dispatch(
        Action::TaskComplete(TaskResult::RewindPreviewFailed {
            agent_id: id,
            error: "timed out".into(),
            target_prompt_index: 1,
            mode: crate::views::rewind::RewindMode::All,
        }),
        &mut app,
    );
    assert!(effects.is_empty(), "got {effects:?}");
    assert!(
        matches!(
            app.agents[&id].rewind_state.as_ref().unwrap().phase,
            crate::views::rewind::RewindPhase::Previewing {
                target_prompt_index: 0,
                ..
            }
        ),
        "the live turn-0 preview must survive the stale failure"
    );

    // The matching failure does land.
    dispatch(
        Action::TaskComplete(TaskResult::RewindPreviewFailed {
            agent_id: id,
            error: "timed out".into(),
            target_prompt_index: 0,
            mode: crate::views::rewind::RewindMode::All,
        }),
        &mut app,
    );
    match &app.agents[&id].rewind_state.as_ref().unwrap().phase {
        crate::views::rewind::RewindPhase::Error { message } => assert_eq!(message, "timed out"),
        other => panic!("expected Error, got {other:?}"),
    }
}

/// An older shell may omit `mode` from the execute response. The phase it was launched
/// from decides what happened, so a files-only rewind still skips the truncation.
#[test]
fn success_without_a_mode_falls_back_to_the_executing_phase() {
    let mut app = app_with_two_turns();
    let id = AgentId(0);
    dispatch(Action::RewindShowPicker, &mut app);
    dispatch(
        points_loaded_with(id, vec![rewind_point_with_files(1, 1), rewind_point(0)]),
        &mut app,
    );
    dispatch(Action::RewindPickerSelect(1), &mut app);
    dispatch(
        Action::RewindSelectMode {
            target: 1,
            mode: crate::views::rewind::RewindMode::FilesOnly,
        },
        &mut app,
    );
    dispatch(
        preview_complete(
            id,
            1,
            crate::views::rewind::RewindMode::FilesOnly,
            preview_response(1, &["src/a.rs"], &[]),
        ),
        &mut app,
    );
    dispatch(Action::RewindConfirm(1), &mut app);

    let len_before = app.agents[&id].scrollback.len();
    dispatch(
        Action::TaskComplete(TaskResult::RewindExecuteComplete {
            agent_id: id,
            response: crate::views::rewind::RewindResponse {
                success: true,
                target_prompt_index: 1,
                reverted_files: vec!["src/a.rs".into()],
                clean_files: vec![],
                conflicts: vec![],
                error: None,
                mode: None,
                prompt_text: Some("turn 1".into()),
            },
        }),
        &mut app,
    );

    let agent = &app.agents[&id];
    assert_eq!(
        agent.scrollback.len(),
        len_before,
        "the Executing phase said files_only, so nothing may be truncated"
    );
    assert_eq!(
        agent.toast.as_ref().map(|(m, _)| m.as_str()),
        Some("Reverted 1 file")
    );
}

/// Confirm is only meaningful from the three phases that offer it. A key or click that
/// lands after the phase has moved on must not start a rewind.
#[test]
fn confirm_outside_a_confirm_phase_does_nothing() {
    let mut app = app_with_tracked_edits();
    let id = AgentId(0);
    dispatch(Action::RewindPickerSelect(1), &mut app);
    dispatch(
        Action::RewindSelectMode {
            target: 1,
            mode: crate::views::rewind::RewindMode::All,
        },
        &mut app,
    );

    // Still Previewing: the dry run has not come back.
    let effects = dispatch(Action::RewindConfirm(1), &mut app);
    assert!(effects.is_empty(), "got {effects:?}");
    assert!(matches!(
        app.agents[&id].rewind_state.as_ref().unwrap().phase,
        crate::views::rewind::RewindPhase::Previewing { .. }
    ));

    // And with no flow at all.
    dispatch(Action::RewindDismiss, &mut app);
    let effects = dispatch(Action::RewindConfirm(1), &mut app);
    assert!(effects.is_empty(), "got {effects:?}");
    assert!(app.agents[&id].rewind_state.is_none());
}
