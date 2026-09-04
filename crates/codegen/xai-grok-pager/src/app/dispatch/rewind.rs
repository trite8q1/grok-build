//! Conversation rewind dispatchers and prompt-entry lookup helpers.

use super::ctx::NO_SESSION_NOTICE;
use crate::app::actions::Effect;
use crate::app::agent::AgentId;
use crate::app::agent_view::AgentView;
use crate::app::app_view::{ActiveView, AppView};
use crate::scrollback::block::RenderBlock;
use crate::scrollback::state::ScrollbackState;
use crate::views::prompt_widget::{PromptWidget, StashedPrompt};
use crate::views::rewind::{
    ConflictDisplay, RewindMode, RewindPhase, RewindPointInfo, RewindState, file_count_phrase,
};

/// Interjections render as user prompts but the shell never numbers them, so counting them would skew the positional prompt-to-entry mapping.
/// Known approximation: an interjection the shell converted into its own `interject-fallback-` turn IS shell-numbered.
/// The positional fallback thus under-counts around it until a resume replays it as an indexed prompt.
fn is_indexed_user_prompt(block: &RenderBlock) -> bool {
    matches!(block, RenderBlock::UserPrompt(b) if !b.is_interjection)
}

fn stash_prompt(prompt: &mut PromptWidget) -> Option<StashedPrompt> {
    if prompt.text().is_empty() {
        None
    } else {
        Some(prompt.stash())
    }
}

/// Whether restoring to `target` would move anything on disk.
///
/// A point's `num_file_snapshots` counts only that prompt's own edits, but a restore to `target`
/// reverts every path touched at prompt index >= `target`, so the question is cumulative.
pub(super) fn has_tracked_edits_from(points: &[RewindPointInfo], target: usize) -> bool {
    points
        .iter()
        .any(|p| p.prompt_index >= target && p.num_file_snapshots > 0)
}

fn agent_has_tracked_edits_from(agent: &AgentView, target: usize) -> bool {
    agent
        .rewind_points
        .as_deref()
        .is_some_and(|points| has_tracked_edits_from(points, target))
}

fn prompt_preview_for(agent: &AgentView, target: usize) -> Option<String> {
    agent
        .rewind_points
        .as_ref()?
        .iter()
        .find(|p| p.prompt_index == target)?
        .prompt_preview
        .clone()
}

pub(in crate::app) fn shell_prompt_index_at(
    scrollback: &ScrollbackState,
    entry_idx: usize,
) -> Option<usize> {
    for idx in (0..=entry_idx).rev() {
        if let Some(e) = scrollback.get(idx)
            && let RenderBlock::UserPrompt(ref block) = e.block
        {
            // A mid-turn interjection belongs to the enclosing turn; keep walking back to that turn's starting prompt
            if block.is_interjection {
                continue;
            }
            if let Some(pi) = block.prompt_index {
                return Some(pi);
            }
            let count = (0..=idx)
                .filter(|&i| {
                    scrollback
                        .get(i)
                        .is_some_and(|e2| is_indexed_user_prompt(&e2.block))
                })
                .count();
            return if count > 0 { Some(count - 1) } else { None };
        }
    }
    None
}

pub(in crate::app) fn find_user_prompt_entry_for_shell_index(
    scrollback: &ScrollbackState,
    target_prompt_index: usize,
) -> Option<usize> {
    for idx in (0..scrollback.len()).rev() {
        if let Some(entry) = scrollback.get(idx)
            && let RenderBlock::UserPrompt(ref block) = entry.block
            && block.prompt_index == Some(target_prompt_index)
        {
            return Some(idx);
        }
    }
    let mut count = 0usize;
    for idx in 0..scrollback.len() {
        if let Some(e) = scrollback.get(idx)
            && is_indexed_user_prompt(&e.block)
        {
            if count == target_prompt_index {
                return Some(idx);
            }
            count += 1;
        }
    }
    None
}

pub(super) fn dispatch_rewind(app: &mut AppView) -> Vec<Effect> {
    let ActiveView::Agent(id) = app.active_view else {
        return vec![];
    };
    let Some(agent) = app.agents.get_mut(&id) else {
        return vec![];
    };
    let Some(session_id) = agent.session.session_id.clone() else {
        app.show_toast(NO_SESSION_NOTICE);
        return vec![];
    };

    // Rewind takes input priority over the `/jump` picker; close a lingering one first so it can't reappear (stale) after rewind finishes
    agent.dismiss_jump_picker();

    let selected_idx = agent.scrollback.selected();
    let selected_shell_idx =
        selected_idx.and_then(|idx| shell_prompt_index_at(&agent.scrollback, idx));

    if agent.session.state.is_busy() {
        let anchor = agent.scrollback.len().saturating_sub(1);
        let draft = stash_prompt(&mut agent.prompt);
        agent.rewind_state = Some(RewindState::new_cancel_offer(
            anchor,
            draft,
            selected_shell_idx,
        ));
        return vec![];
    }

    let draft = stash_prompt(&mut agent.prompt);
    agent.rewind_state = Some(RewindState {
        phase: RewindPhase::Loading,
        anchor_entry_idx: selected_idx.unwrap_or(0),
        stashed_draft: draft,
        selected_prompt_index: selected_shell_idx,
    });

    vec![Effect::FetchRewindPoints {
        agent_id: id,
        session_id,
    }]
}

pub(super) fn dispatch_rewind_show_picker(app: &mut AppView) -> Vec<Effect> {
    let ActiveView::Agent(id) = app.active_view else {
        return vec![];
    };
    let Some(agent) = app.agents.get_mut(&id) else {
        return vec![];
    };
    let Some(session_id) = agent.session.session_id.clone() else {
        app.show_toast(NO_SESSION_NOTICE);
        return vec![];
    };

    // Rewind takes input priority over the `/jump` picker; close a lingering one first so it can't reappear (stale) after rewind finishes
    agent.dismiss_jump_picker();

    if agent.session.state.is_busy() {
        let anchor = agent.scrollback.len().saturating_sub(1);
        let draft = stash_prompt(&mut agent.prompt);
        agent.rewind_state = Some(RewindState::new_cancel_offer(anchor, draft, None));
        return vec![];
    }

    let draft = stash_prompt(&mut agent.prompt);
    agent.rewind_state = Some(RewindState {
        phase: RewindPhase::Loading,
        anchor_entry_idx: 0,
        stashed_draft: draft,
        selected_prompt_index: None,
    });

    vec![Effect::FetchRewindPoints {
        agent_id: id,
        session_id,
    }]
}

pub(super) fn dispatch_rewind_picker_select(app: &mut AppView, prompt_index: usize) -> Vec<Effect> {
    let ActiveView::Agent(id) = app.active_view else {
        return vec![];
    };
    let Some(agent) = app.agents.get_mut(&id) else {
        return vec![];
    };

    let preview = prompt_preview_for(agent, prompt_index);
    let has_file_changes = agent_has_tracked_edits_from(agent, prompt_index);
    let offer_files_only = agent.inline_edit.is_none();

    let anchor = find_user_prompt_entry_for_shell_index(&agent.scrollback, prompt_index);
    if let Some(entry_idx) = anchor {
        agent.scrollback.set_selected(Some(entry_idx));
    }

    let draft = agent.rewind_state.take().and_then(|s| s.stashed_draft);
    open_mode_select(
        agent,
        prompt_index,
        anchor.unwrap_or(0),
        draft,
        preview,
        has_file_changes,
        offer_files_only,
    );
    vec![]
}

/// The turn is picked and a mode chosen. Anything that touches files goes through a dry-run
/// preview first; a pure conversation rewind is gated by the confirm setting (or the
/// prompt-0 orphan warning) and otherwise executes straight away.
pub(super) fn dispatch_rewind_select_mode(
    app: &mut AppView,
    target: usize,
    mode: RewindMode,
) -> Vec<Effect> {
    let ActiveView::Agent(id) = app.active_view else {
        return vec![];
    };
    let confirm = app.current_ui.confirm_before_rewind_enabled();
    let Some(agent) = app.agents.get_mut(&id) else {
        return vec![];
    };

    let (has_file_changes, offer_files_only) = match agent.rewind_state.as_ref().map(|s| &s.phase) {
        Some(RewindPhase::ModeSelect {
            has_file_changes,
            offer_files_only,
            ..
        }) => (*has_file_changes, *offer_files_only),
        _ => (
            agent_has_tracked_edits_from(agent, target),
            agent.inline_edit.is_none(),
        ),
    };

    // The files-only row is dim (no tracked edits) or absent (inline resubmit) in these cases,
    // so a stray key or click must not slip past it.
    if mode == RewindMode::FilesOnly && (!has_file_changes || !offer_files_only) {
        return vec![];
    }

    let anchor = agent
        .rewind_state
        .as_ref()
        .map(|s| s.anchor_entry_idx)
        .unwrap_or(0);
    let preview = prompt_preview_for(agent, target);

    // `All` with nothing tracked on disk moves no files, so it takes the conversation path below.
    // The wire mode stays `All` either way.
    let touches_files = match mode {
        RewindMode::FilesOnly => true,
        RewindMode::All => has_file_changes,
        RewindMode::ConversationOnly => false,
    };

    let draft = agent.rewind_state.take().and_then(|s| s.stashed_draft);

    if touches_files {
        let Some(session_id) = agent.session.session_id.clone() else {
            if let Some(d) = draft {
                agent.prompt.restore(d);
            }
            agent.rewind_state = None;
            agent.rewind_points = None;
            return vec![];
        };
        agent.rewind_state = Some(RewindState {
            phase: RewindPhase::Previewing {
                target_prompt_index: target,
                mode,
            },
            anchor_entry_idx: anchor,
            stashed_draft: draft,
            selected_prompt_index: Some(target),
        });
        return vec![Effect::RewindPreview {
            agent_id: id,
            session_id,
            target_prompt_index: target,
            mode,
        }];
    }

    if target == 0 && has_file_changes {
        // Rewinding the conversation to prompt 0 clears every file snapshot, so the discarded
        // turns' edits stay on disk with no later files-only rewind able to undo them.
        agent.rewind_state = Some(RewindState {
            phase: RewindPhase::OrphanWarning {
                target_prompt_index: target,
                active_idx: 0,
                prompt_preview: preview,
            },
            anchor_entry_idx: anchor,
            stashed_draft: draft,
            selected_prompt_index: Some(target),
        });
        return vec![];
    }

    if confirm {
        agent.rewind_state = Some(RewindState {
            phase: RewindPhase::Confirm {
                target_prompt_index: target,
                mode,
                active_idx: 0,
                prompt_preview: preview,
            },
            anchor_entry_idx: anchor,
            stashed_draft: draft,
            selected_prompt_index: Some(target),
        });
        return vec![];
    }

    enter_executing(agent, id, target, anchor, draft, mode)
}

pub(super) fn dispatch_rewind_back_to_mode_select(app: &mut AppView) -> Vec<Effect> {
    let ActiveView::Agent(id) = app.active_view else {
        return vec![];
    };
    let Some(agent) = app.agents.get_mut(&id) else {
        return vec![];
    };
    back_to_mode_select(agent);
    vec![]
}

pub(super) fn dispatch_rewind_cancel_offer(app: &mut AppView) -> Vec<Effect> {
    let ActiveView::Agent(id) = app.active_view else {
        return vec![];
    };
    let Some(agent) = app.agents.get_mut(&id) else {
        return vec![];
    };
    let Some(session_id) = agent.session.session_id.clone() else {
        return vec![];
    };

    let anchor = agent
        .rewind_state
        .as_ref()
        .map(|s| s.anchor_entry_idx)
        .unwrap_or(0);
    let selected = agent
        .rewind_state
        .as_ref()
        .and_then(|s| s.selected_prompt_index);
    let draft = agent.rewind_state.take().and_then(|s| s.stashed_draft);
    agent.rewind_state = Some(RewindState {
        phase: RewindPhase::Loading,
        anchor_entry_idx: anchor,
        stashed_draft: draft,
        selected_prompt_index: selected,
    });
    let mut effects = vec![Effect::CancelTurn {
        session_id: session_id.clone(),
        cancel_subagents: true,
        trigger: None,
        // The rewind picker owns history via `handle_rewind`; this pre-cancel must not also pop the in-flight prompt
        rewind_prompt_id: None,
    }];
    effects.push(Effect::FetchRewindPoints {
        agent_id: id,
        session_id,
    });
    effects
}

pub(super) fn dispatch_rewind_confirm(app: &mut AppView, target: usize) -> Vec<Effect> {
    let ActiveView::Agent(id) = app.active_view else {
        return vec![];
    };
    let Some(agent) = app.agents.get_mut(&id) else {
        return vec![];
    };
    // Only the three confirm-style phases can commit. Anything else (a stale key, a click
    // landing after the phase moved on) leaves the flow exactly as it is.
    let (anchor, mode) = match agent.rewind_state.as_ref() {
        Some(s) => match s.phase {
            RewindPhase::FilePreview { mode, .. } | RewindPhase::Confirm { mode, .. } => {
                (s.anchor_entry_idx, mode)
            }
            // The orphan warning only ever guards a conversation-only rewind.
            RewindPhase::OrphanWarning { .. } => (s.anchor_entry_idx, RewindMode::ConversationOnly),
            _ => return vec![],
        },
        None => return vec![],
    };
    let draft = agent.rewind_state.take().and_then(|s| s.stashed_draft);
    enter_executing(agent, id, target, anchor, draft, mode)
}

/// "Yes, and don't ask again": quiet-persist confirm-before-rewind off, then execute.
/// No settings checkmark toast; success/toast comes from the rewind itself.
pub(super) fn dispatch_rewind_confirm_never_ask(app: &mut AppView, target: usize) -> Vec<Effect> {
    // The rewind goes first: if the phase can't commit, the setting must not change either.
    let mut effects = dispatch_rewind_confirm(app, target);
    if effects.is_empty() {
        return effects;
    }
    if app.current_ui.confirm_before_rewind_enabled() {
        super::settings::setters::set_confirm_before_rewind_inner(app, false);
        super::settings::ui::refresh_open_settings_modals(app);
        effects.push(Effect::PersistSetting {
            key: "confirm_before_rewind",
            value: crate::settings::SettingValue::Bool(false),
            rollback_value: crate::settings::SettingValue::Bool(true),
        });
    }
    effects
}

pub(super) fn dispatch_rewind_dismiss(app: &mut AppView) -> Vec<Effect> {
    let ActiveView::Agent(id) = app.active_view else {
        return vec![];
    };
    let Some(agent) = app.agents.get_mut(&id) else {
        return vec![];
    };
    let draft = agent.rewind_state.take().and_then(|s| s.stashed_draft);
    if let Some(d) = draft {
        agent.prompt.restore(d);
    }
    agent.rewind_points = None;
    vec![]
}

pub(super) fn dispatch_rewind_dismiss_error(app: &mut AppView) -> Vec<Effect> {
    dispatch_rewind_dismiss(app)
}

/// The single place the inline-edit resubmit gets set: called right before every `Effect::RewindExecute` emission in the rewind flow.
/// If the inline editor is open, the (trimmed) edited text is stashed for `dispatch_rewind_success` to resubmit after the rewind lands.
/// Dismiss / error / empty-points paths never set it, so they need no clearing; the editor stays open there.
fn stash_inline_resubmit_if_editing(agent: &mut AgentView) {
    if let Some(ref edit) = agent.inline_edit {
        agent.pending_inline_resubmit = Some(edit.textarea.text().trim().to_string());
    }
}

/// Enter `Executing` and emit `RewindExecute` (shared by every confirm row and by the
/// immediate execute when confirm-before-rewind is off).
fn enter_executing(
    agent: &mut AgentView,
    agent_id: AgentId,
    target: usize,
    anchor: usize,
    draft: Option<StashedPrompt>,
    mode: RewindMode,
) -> Vec<Effect> {
    let Some(session_id) = agent.session.session_id.clone() else {
        if let Some(d) = draft {
            agent.prompt.restore(d);
        }
        agent.rewind_state = None;
        agent.rewind_points = None;
        return vec![];
    };
    agent.rewind_state = Some(RewindState {
        phase: RewindPhase::Executing {
            target_prompt_index: target,
            mode,
        },
        anchor_entry_idx: anchor,
        stashed_draft: draft,
        selected_prompt_index: None,
    });
    stash_inline_resubmit_if_editing(agent);
    vec![Effect::RewindExecute {
        agent_id,
        session_id,
        target_prompt_index: target,
        mode,
    }]
}

fn open_mode_select(
    agent: &mut AgentView,
    target: usize,
    anchor: usize,
    draft: Option<StashedPrompt>,
    prompt_preview: Option<String>,
    has_file_changes: bool,
    offer_files_only: bool,
) {
    agent.rewind_state = Some(RewindState {
        phase: RewindPhase::ModeSelect {
            target_prompt_index: target,
            has_file_changes,
            offer_files_only,
            active_idx: 0,
            prompt_preview,
        },
        anchor_entry_idx: anchor,
        stashed_draft: draft,
        selected_prompt_index: Some(target),
    });
}

/// Back out of a confirm-style phase to a fresh mode select.
/// The facts are re-derived from `rewind_points` and the live inline-edit state rather than
/// carried through every phase.
fn back_to_mode_select(agent: &mut AgentView) {
    let Some(state) = agent.rewind_state.take() else {
        return;
    };
    let target = match state.phase {
        RewindPhase::FilePreview {
            target_prompt_index,
            ..
        }
        | RewindPhase::Confirm {
            target_prompt_index,
            ..
        }
        | RewindPhase::OrphanWarning {
            target_prompt_index,
            ..
        } => target_prompt_index,
        _ => {
            agent.rewind_state = Some(state);
            return;
        }
    };
    let preview = prompt_preview_for(agent, target);
    let has_file_changes = agent_has_tracked_edits_from(agent, target);
    let offer_files_only = agent.inline_edit.is_none();
    open_mode_select(
        agent,
        target,
        state.anchor_entry_idx,
        state.stashed_draft,
        preview,
        has_file_changes,
        offer_files_only,
    );
}

pub(super) fn dispatch_inline_edit_submit(app: &mut AppView) -> Vec<Effect> {
    let ActiveView::Agent(id) = app.active_view else {
        return vec![];
    };
    let Some(agent) = app.agents.get_mut(&id) else {
        return vec![];
    };
    let Some(session_id) = agent.session.session_id.clone() else {
        app.show_toast(NO_SESSION_NOTICE);
        return vec![];
    };
    let Some(edit) = agent.inline_edit.as_ref() else {
        return vec![];
    };

    // Unchanged/empty edits have nothing to submit: just close the editor.
    let text = edit.textarea.text().trim().to_string();
    if text.is_empty() || text == edit.original.trim() {
        agent.exit_inline_edit();
        return vec![];
    }

    let target = edit.prompt_index;
    let anchor = agent
        .scrollback
        .index_of_id(edit.entry_id)
        .or_else(|| agent.scrollback.selected())
        .unwrap_or(0);
    let draft = stash_prompt(&mut agent.prompt);

    if agent.session.state.is_busy() {
        // Mid-turn submit: the same cancel offer `/rewind` raises appears over the still-open editor
        // Confirm cancels the turn and re-enters the flow; dismiss returns to the editor
        agent.rewind_state = Some(RewindState::new_cancel_offer(anchor, draft, Some(target)));
        return vec![];
    }

    agent.rewind_state = Some(RewindState {
        phase: RewindPhase::Loading,
        anchor_entry_idx: anchor,
        stashed_draft: draft,
        selected_prompt_index: Some(target),
    });

    vec![Effect::FetchRewindPoints {
        agent_id: id,
        session_id,
    }]
}

pub(super) fn dispatch_rewind_success(
    app: &mut AppView,
    agent_id: crate::app::agent::AgentId,
    response: crate::views::rewind::RewindResponse,
) -> Vec<Effect> {
    let Some(agent) = app.agents.get_mut(&agent_id) else {
        return vec![];
    };

    // Inline-edit resubmit text; taken unconditionally so a failed rewind drops it
    let inline_resubmit = agent.pending_inline_resubmit.take();

    if !response.success {
        let err = response.error.unwrap_or_else(|| "unknown error".into());
        let anchor = agent
            .rewind_state
            .as_ref()
            .map(|s| s.anchor_entry_idx)
            .unwrap_or(0);
        let draft = agent.rewind_state.take().and_then(|s| s.stashed_draft);
        agent.rewind_state = Some(RewindState {
            phase: RewindPhase::Error { message: err },
            anchor_entry_idx: anchor,
            stashed_draft: draft,
            selected_prompt_index: None,
        });
        // The inline editor (if any) stays open; dismissing the error returns to editing
        return vec![];
    }

    // The rewind went through: the inline editor's job is done. Close it before the truncation below removes its entry.
    if inline_resubmit.is_some() {
        agent.inline_edit = None;
        agent.scrollback.set_inline_edit_height(None);
    }

    let target = response.target_prompt_index;
    // The response echoes the mode it applied; the phase it was launched from is the fallback.
    let executing_mode = agent.rewind_state.as_ref().and_then(|s| match s.phase {
        RewindPhase::Executing { mode, .. } => Some(mode),
        _ => None,
    });
    let stashed_draft = agent.rewind_state.take().and_then(|s| s.stashed_draft);
    let mode = response
        .mode
        .as_deref()
        .map(RewindMode::from_wire)
        .unwrap_or(executing_mode.unwrap_or(RewindMode::ConversationOnly));
    let rewinds_conversation = matches!(mode, RewindMode::All | RewindMode::ConversationOnly);

    if rewinds_conversation {
        // The summary describes turns the rewind just removed (the shell clears its persisted copy on the same branch)
        // Bump gen so a late SessionMetaFromDisk hydrate cannot restore the pre-rewind summary.json value into the cleared field
        agent.set_last_turn_summary(None);
        let target_idx = find_user_prompt_entry_for_shell_index(&agent.scrollback, target);
        if let Some(anchor_idx) = target_idx {
            let removed = agent.scrollback.remove_from(anchor_idx);
            // Explicit drop BEFORE the purge: the rewound tail must be freed for the release below to return its pages
            drop(removed);
            crate::memory_release::release_retained_memory("rewind-truncate");
        }
    }

    // An inline resubmit skips the confirmation; the edited prompt re-appearing at the same spot is self-explanatory
    // A file revert still needs its own signal, since nothing else on screen shows it happened
    let reverted = response.reverted_files.len();
    if inline_resubmit.is_none() || reverted > 0 {
        let msg = if inline_resubmit.is_some() {
            format!("Reverted {}", file_count_phrase(reverted))
        } else {
            match mode {
                RewindMode::ConversationOnly => "Reverted conversation".to_string(),
                RewindMode::FilesOnly => format!("Reverted {}", file_count_phrase(reverted)),
                RewindMode::All if reverted == 0 => "Reverted conversation".to_string(),
                RewindMode::All => {
                    format!("Reverted conversation and {}", file_count_phrase(reverted))
                }
            }
        };
        if app.screen_mode.is_minimal() {
            // Minimal has no toast area and can't erase committed lines, so the confirmation stays in scrollback there
            agent.scrollback.push_block(RenderBlock::system(msg));
        } else {
            agent.show_toast(&msg);
        }
    }

    if inline_resubmit.is_some() {
        // Restore the full draft before a non-consuming resubmit.
        if let Some(draft) = stashed_draft {
            agent.prompt.restore(draft);
        }
    } else if rewinds_conversation {
        if let Some(ref prompt_text) = response.prompt_text {
            agent.prompt.set_text(prompt_text);
        } else if let Some(draft) = stashed_draft {
            agent.prompt.restore(draft);
        }
    } else if let Some(draft) = stashed_draft {
        // Files only: the composer was never the subject, so the draft comes straight back.
        agent.prompt.restore(draft);
    }

    if rewinds_conversation {
        agent.set_active_pane(crate::app::agent_view::ActivePane::Prompt, false);
    }

    agent.rewind_points = None;
    // A files-only rewind leaves the transcript alone, so it must leave the viewport alone too
    if rewinds_conversation {
        agent.scrollback.goto_bottom();
    }

    if let Some(text) = inline_resubmit {
        if app.active_view == ActiveView::Agent(agent_id) {
            // Resubmit from the rewound point; `consume_input=false` keeps the composer draft, `literal=true` sends slash-lookalike text as a prompt
            // (The transcript is already truncated; running it as a command would swallow the resubmit.)
            return super::prompt::dispatch_send_prompt_inner(
                app, text, /* consume_input */ false, /* literal */ true,
                /* is_follow_up */ false,
            );
        }
        // View switched mid-rewind: fall back to prefilling that composer, appending so an existing draft isn't clobbered
        if let Some(agent) = app.agents.get_mut(&agent_id) {
            if agent.prompt.text().trim().is_empty() {
                agent.prompt.set_text(&text);
            } else {
                agent.prompt.append_text(&format!("\n{text}"));
            }
        }
    }

    vec![]
}

// TaskResult handlers.

pub(super) fn handle_rewind_points_loaded(
    app: &mut AppView,
    agent_id: AgentId,
    points: Vec<RewindPointInfo>,
) -> Vec<Effect> {
    let Some(agent) = app.agents.get_mut(&agent_id) else {
        return vec![];
    };
    agent.rewind_points = Some(points.clone());

    let desired_target = agent
        .rewind_state
        .as_ref()
        .and_then(|s| s.selected_prompt_index);
    let stashed = agent.rewind_state.take().and_then(|s| s.stashed_draft);

    if points.is_empty() {
        if let Some(stashed) = stashed {
            agent.prompt.restore(stashed);
        }
        app.show_toast("No undoable prompts");
        return vec![];
    }

    if let Some(dt) = desired_target {
        let resolved = points
            .iter()
            .find(|p| p.prompt_index == dt)
            .or_else(|| points.iter().max_by_key(|p| p.prompt_index))
            .cloned();

        if let Some(point) = resolved {
            let target = point.prompt_index;
            let preview = point.prompt_preview.clone();
            let has_file_changes = has_tracked_edits_from(&points, target);
            // Inline edit-and-resubmit: the conversation rewind is a given, so hide the files-only row
            let offer_files_only = agent.inline_edit.is_none();
            let anchor = find_user_prompt_entry_for_shell_index(&agent.scrollback, target);
            let draft = stashed.or_else(|| stash_prompt(&mut agent.prompt));
            if let Some(entry_idx) = anchor {
                agent.scrollback.set_selected(Some(entry_idx));
            }
            open_mode_select(
                agent,
                target,
                anchor.unwrap_or(0),
                draft,
                preview,
                has_file_changes,
                offer_files_only,
            );
            return vec![];
        }
    }

    let mut sorted = points;
    sorted.sort_by(|a, b| b.prompt_index.cmp(&a.prompt_index));
    let draft = stashed.or_else(|| stash_prompt(&mut agent.prompt));
    let initial_anchor = sorted
        .first()
        .map(|p| {
            find_user_prompt_entry_for_shell_index(&agent.scrollback, p.prompt_index).unwrap_or(0)
        })
        .unwrap_or(0);
    agent.rewind_state = Some(RewindState {
        phase: RewindPhase::Picker {
            points: sorted,
            selected: 0,
        },
        anchor_entry_idx: initial_anchor,
        stashed_draft: draft,
        selected_prompt_index: None,
    });
    agent.scrollback.scroll_to_entry_center(initial_anchor);
    vec![]
}

/// Whether a preview result still belongs to the flow on screen.
/// Esc during the dry run drops the whole flow, and a late result must not resurrect it.
fn awaiting_preview(agent: &AgentView, target: usize, mode: RewindMode) -> bool {
    matches!(
        agent.rewind_state.as_ref().map(|s| &s.phase),
        Some(RewindPhase::Previewing {
            target_prompt_index,
            mode: m,
        }) if *target_prompt_index == target && *m == mode
    )
}

pub(super) fn handle_rewind_preview_complete(
    app: &mut AppView,
    agent_id: AgentId,
    response: crate::views::rewind::RewindResponse,
    target_prompt_index: usize,
    mode: RewindMode,
) -> Vec<Effect> {
    let Some(agent) = app.agents.get_mut(&agent_id) else {
        return vec![];
    };
    if !awaiting_preview(agent, target_prompt_index, mode) {
        return vec![];
    }
    let anchor = agent
        .rewind_state
        .as_ref()
        .map(|s| s.anchor_entry_idx)
        .unwrap_or(0);

    // "External modifications detected" is not a failure here; those conflicts are shown as rows.
    // A real error (an invalid target, say) comes back with both lists empty.
    if response.error.is_some() && response.clean_files.is_empty() && response.conflicts.is_empty()
    {
        let err = response.error.unwrap_or_default();
        let draft = agent.rewind_state.take().and_then(|s| s.stashed_draft);
        agent.rewind_state = Some(RewindState {
            phase: RewindPhase::Error { message: err },
            anchor_entry_idx: anchor,
            stashed_draft: draft,
            selected_prompt_index: None,
        });
        return vec![];
    }

    let conflicts: Vec<ConflictDisplay> = response
        .conflicts
        .iter()
        .map(ConflictDisplay::from_conflict)
        .collect();
    let preview = prompt_preview_for(agent, target_prompt_index);
    let draft = agent.rewind_state.take().and_then(|s| s.stashed_draft);
    agent.rewind_state = Some(RewindState {
        phase: RewindPhase::FilePreview {
            target_prompt_index,
            mode,
            clean_files: response.clean_files,
            conflicts,
            active_idx: 0,
            prompt_preview: preview,
        },
        anchor_entry_idx: anchor,
        stashed_draft: draft,
        selected_prompt_index: Some(target_prompt_index),
    });
    vec![]
}

pub(super) fn handle_rewind_preview_failed(
    app: &mut AppView,
    agent_id: AgentId,
    error: String,
    target_prompt_index: usize,
    mode: RewindMode,
) -> Vec<Effect> {
    let Some(agent) = app.agents.get_mut(&agent_id) else {
        return vec![];
    };
    // Same guard as the success path: an abandoned dry run timing out must not
    // replace the flow the user has since reopened.
    if !awaiting_preview(agent, target_prompt_index, mode) {
        return vec![];
    }
    let anchor = agent
        .rewind_state
        .as_ref()
        .map(|s| s.anchor_entry_idx)
        .unwrap_or(0);
    let draft = agent.rewind_state.take().and_then(|s| s.stashed_draft);
    agent.rewind_state = Some(RewindState {
        phase: RewindPhase::Error { message: error },
        anchor_entry_idx: anchor,
        stashed_draft: draft,
        selected_prompt_index: None,
    });
    vec![]
}

pub(super) fn handle_rewind_execute_failed(
    app: &mut AppView,
    agent_id: AgentId,
    error: String,
) -> Vec<Effect> {
    let Some(agent) = app.agents.get_mut(&agent_id) else {
        return vec![];
    };
    // A pending inline resubmit dies with its rewind; the editor itself stays open so dismissing the error returns to editing
    agent.pending_inline_resubmit = None;
    let anchor = agent
        .rewind_state
        .as_ref()
        .map(|s| s.anchor_entry_idx)
        .unwrap_or(0);
    let draft = agent.rewind_state.take().and_then(|s| s.stashed_draft);
    agent.rewind_state = Some(RewindState {
        phase: RewindPhase::Error { message: error },
        anchor_entry_idx: anchor,
        stashed_draft: draft,
        selected_prompt_index: None,
    });
    vec![]
}
