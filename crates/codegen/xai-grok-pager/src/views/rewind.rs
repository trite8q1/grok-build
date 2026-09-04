use crossterm::event::{KeyCode, KeyEvent};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};

use crate::theme::Theme;
use crate::views::prompt_widget::StashedPrompt;

#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize)]
pub struct RewindPointInfo {
    #[serde(alias = "promptIndex")]
    pub prompt_index: usize,
    #[serde(default, alias = "createdAt")]
    pub created_at: String,
    #[serde(default, alias = "numFileSnapshots")]
    pub num_file_snapshots: usize,
    #[serde(default, alias = "promptPreview")]
    pub prompt_preview: Option<String>,
    #[serde(default, alias = "hasFileChanges")]
    pub has_file_changes: bool,
}

#[derive(Debug, Clone, serde::Deserialize)]
pub struct RewindPointsResponse {
    #[serde(alias = "rewindPoints")]
    pub rewind_points: Vec<RewindPointInfo>,
}

#[derive(Debug, Clone, serde::Deserialize)]
pub struct RewindResponse {
    pub success: bool,
    #[serde(alias = "targetPromptIndex")]
    pub target_prompt_index: usize,
    #[serde(default, alias = "revertedFiles")]
    pub reverted_files: Vec<String>,
    #[serde(default, alias = "cleanFiles")]
    pub clean_files: Vec<String>,
    #[serde(default)]
    pub conflicts: Vec<RewindConflictInfo>,
    #[serde(default)]
    pub error: Option<String>,
    #[serde(default)]
    pub mode: Option<String>,
    #[serde(default, alias = "promptText")]
    pub prompt_text: Option<String>,
}

#[derive(Debug, Clone, serde::Deserialize)]
pub struct RewindConflictInfo {
    pub path: String,
    #[serde(alias = "conflictType")]
    pub conflict_type: String,
}

/// One conflicting path plus the short label the preview list shows next to it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConflictDisplay {
    pub path: String,
    pub label: &'static str,
}

impl ConflictDisplay {
    pub fn from_conflict(c: &RewindConflictInfo) -> Self {
        let label = match c.conflict_type.as_str() {
            "deleted_externally" => "deleted",
            "created_externally" => "added",
            "modified_externally" => "modified",
            _ => "conflict",
        };
        Self {
            path: c.path.clone(),
            label,
        }
    }
}

/// What `/rewind` should restore. Wire values match `x.ai/rewind/execute`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RewindMode {
    /// Conversation and files.
    All,
    ConversationOnly,
    FilesOnly,
}

impl RewindMode {
    pub fn as_wire(self) -> &'static str {
        match self {
            Self::All => "all",
            Self::ConversationOnly => "conversation_only",
            Self::FilesOnly => "files_only",
        }
    }

    pub fn from_wire(s: &str) -> Self {
        match s {
            "all" => Self::All,
            "files_only" | "code_only" => Self::FilesOnly,
            _ => Self::ConversationOnly,
        }
    }
}

/// "1 file" / "N files", for picker rows and rewind confirmations.
pub fn file_count_phrase(n: usize) -> String {
    if n == 1 {
        "1 file".to_string()
    } else {
        format!("{n} files")
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RewindPhase {
    Loading,
    Picker {
        points: Vec<RewindPointInfo>,
        selected: usize,
    },
    CancelOffer {
        active_idx: usize,
    },
    /// Both / conversation only / files only, after a turn is picked.
    ModeSelect {
        target_prompt_index: usize,
        has_file_changes: bool,
        /// When false, the files-only row is omitted (inline edit-and-resubmit).
        offer_files_only: bool,
        active_idx: usize,
        prompt_preview: Option<String>,
    },
    /// A `force: false` dry run is in flight; nothing has been written yet.
    Previewing {
        target_prompt_index: usize,
        mode: RewindMode,
    },
    /// The dry run came back: the files that would move, plus any external conflicts.
    FilePreview {
        target_prompt_index: usize,
        mode: RewindMode,
        clean_files: Vec<String>,
        conflicts: Vec<ConflictDisplay>,
        active_idx: usize,
        prompt_preview: Option<String>,
    },
    /// The `confirm_before_rewind` gate, for rewinds that touch no files.
    Confirm {
        target_prompt_index: usize,
        mode: RewindMode,
        active_idx: usize,
        prompt_preview: Option<String>,
    },
    /// Conversation-only at prompt 0 with tracked edits: rewinding to 0 clears every file
    /// snapshot, so the discarded turns' edits stay on disk with no way back.
    OrphanWarning {
        target_prompt_index: usize,
        active_idx: usize,
        prompt_preview: Option<String>,
    },
    Executing {
        target_prompt_index: usize,
        mode: RewindMode,
    },
    Error {
        message: String,
    },
}

#[derive(Debug)]
pub struct RewindState {
    pub phase: RewindPhase,
    pub anchor_entry_idx: usize,
    pub stashed_draft: Option<StashedPrompt>,
    pub selected_prompt_index: Option<usize>,
}

impl RewindState {
    pub fn new_cancel_offer(
        anchor: usize,
        draft: Option<StashedPrompt>,
        selected_prompt_index: Option<usize>,
    ) -> Self {
        Self {
            phase: RewindPhase::CancelOffer { active_idx: 0 },
            anchor_entry_idx: anchor,
            stashed_draft: draft,
            selected_prompt_index,
        }
    }
}

pub enum RewindInput {
    Dismissed,
    CancelTurnThenProceed,
    DismissError,
    Confirm(usize),
    /// Execute this rewind and turn off confirm-before-rewind.
    ConfirmNeverAsk(usize),
    PickerSelect(usize),
    SelectMode(RewindMode, usize),
    BackToModeSelect,
    MoveUp,
    MoveDown,
    ConfirmCursor,
    Consumed,
}

const CANCEL_OFFER_OPTIONS: usize = 2;
/// Yes / Yes, and don't ask again / No.
const CONFIRM_OPTIONS: usize = 3;
/// Confirm rewind / Back.
const CONFIRM_BACK_OPTIONS: usize = 2;

fn mode_select_count(offer_files_only: bool) -> usize {
    if offer_files_only { 3 } else { 2 }
}

fn mode_for_idx(idx: usize) -> RewindMode {
    match idx {
        0 => RewindMode::All,
        1 => RewindMode::ConversationOnly,
        _ => RewindMode::FilesOnly,
    }
}

/// Rows the file list occupies in `FilePreview`, plus the blank spacer that follows it.
///
/// IMPORTANT: `render_rewind_overlay`, `rewind_row_at`, and `rewind_overlay_height` all read the
/// FilePreview geometry from here. Keep the render loop in step with it.
fn file_preview_rows(clean_files: &[String], conflicts: &[ConflictDisplay]) -> (u16, u16) {
    if clean_files.is_empty() && conflicts.is_empty() {
        // A single gray "nothing to restore" line, with no spacer after it.
        return (1, 0);
    }
    let clean = clean_files.len().min(5) + usize::from(clean_files.len() > 5);
    let conflict = conflicts.len().min(5) + usize::from(conflicts.len() > 5);
    ((clean + conflict) as u16, 1)
}

pub fn handle_rewind_key(state: &RewindState, key: &KeyEvent) -> RewindInput {
    if key.kind == crossterm::event::KeyEventKind::Release {
        return RewindInput::Consumed;
    }
    match &state.phase {
        RewindPhase::Picker { points, selected } => match key.code {
            KeyCode::Char('j') | KeyCode::Down => RewindInput::MoveDown,
            KeyCode::Char('k') | KeyCode::Up => RewindInput::MoveUp,
            KeyCode::Enter => {
                if let Some(p) = points.get(*selected) {
                    RewindInput::PickerSelect(p.prompt_index)
                } else {
                    RewindInput::Consumed
                }
            }
            KeyCode::Esc => RewindInput::Dismissed,
            _ => RewindInput::Consumed,
        },
        RewindPhase::ModeSelect {
            target_prompt_index,
            has_file_changes,
            offer_files_only,
            ..
        } => match key.code {
            KeyCode::Char('j') | KeyCode::Down => RewindInput::MoveDown,
            KeyCode::Char('k') | KeyCode::Up => RewindInput::MoveUp,
            KeyCode::Char('a') => RewindInput::SelectMode(RewindMode::All, *target_prompt_index),
            KeyCode::Char('c') => {
                RewindInput::SelectMode(RewindMode::ConversationOnly, *target_prompt_index)
            }
            // The two-row inline variant letters its rows a/b; 'c' stays as an alias so
            // classic-flow muscle memory keeps working.
            KeyCode::Char('b') if !*offer_files_only => {
                RewindInput::SelectMode(RewindMode::ConversationOnly, *target_prompt_index)
            }
            KeyCode::Char('f') if *offer_files_only && *has_file_changes => {
                RewindInput::SelectMode(RewindMode::FilesOnly, *target_prompt_index)
            }
            KeyCode::Enter => RewindInput::ConfirmCursor,
            KeyCode::Esc => RewindInput::Dismissed,
            _ => RewindInput::Consumed,
        },
        RewindPhase::CancelOffer { .. } => match key.code {
            KeyCode::Char('y') => RewindInput::CancelTurnThenProceed,
            KeyCode::Char('n') => RewindInput::Dismissed,
            KeyCode::Char('j') | KeyCode::Down => RewindInput::MoveDown,
            KeyCode::Char('k') | KeyCode::Up => RewindInput::MoveUp,
            KeyCode::Enter => RewindInput::ConfirmCursor,
            KeyCode::Esc => RewindInput::Dismissed,
            _ => RewindInput::Consumed,
        },
        RewindPhase::FilePreview {
            target_prompt_index,
            ..
        }
        | RewindPhase::OrphanWarning {
            target_prompt_index,
            ..
        } => match key.code {
            KeyCode::Char('y') => RewindInput::Confirm(*target_prompt_index),
            KeyCode::Char('j') | KeyCode::Down => RewindInput::MoveDown,
            KeyCode::Char('k') | KeyCode::Up => RewindInput::MoveUp,
            KeyCode::Enter => RewindInput::ConfirmCursor,
            // Esc dismisses every phase; Backspace is the "back" gesture.
            KeyCode::Backspace => RewindInput::BackToModeSelect,
            KeyCode::Esc => RewindInput::Dismissed,
            _ => RewindInput::Consumed,
        },
        RewindPhase::Confirm {
            target_prompt_index,
            ..
        } => match key.code {
            KeyCode::Char('y') => RewindInput::Confirm(*target_prompt_index),
            KeyCode::Char('n') => RewindInput::Dismissed,
            KeyCode::Char('a') => RewindInput::ConfirmNeverAsk(*target_prompt_index),
            KeyCode::Char('j') | KeyCode::Down => RewindInput::MoveDown,
            KeyCode::Char('k') | KeyCode::Up => RewindInput::MoveUp,
            KeyCode::Enter => RewindInput::ConfirmCursor,
            KeyCode::Backspace => RewindInput::BackToModeSelect,
            KeyCode::Esc => RewindInput::Dismissed,
            _ => RewindInput::Consumed,
        },
        RewindPhase::Error { .. } => match key.code {
            KeyCode::Esc | KeyCode::Enter => RewindInput::DismissError,
            _ => RewindInput::Consumed,
        },
        RewindPhase::Loading | RewindPhase::Previewing { .. } => match key.code {
            KeyCode::Esc => RewindInput::Dismissed,
            _ => RewindInput::Consumed,
        },
        RewindPhase::Executing { .. } => RewindInput::Consumed,
    }
}

pub fn move_cursor(phase: &mut RewindPhase, delta: i32) {
    match phase {
        RewindPhase::Picker { points, selected } => {
            if points.is_empty() {
                return;
            }
            let max = points.len() as i32 - 1;
            let new = (*selected as i32 + delta).clamp(0, max);
            *selected = new as usize;
        }
        RewindPhase::CancelOffer { active_idx } => {
            let new = (*active_idx as i32 + delta).clamp(0, CANCEL_OFFER_OPTIONS as i32 - 1);
            *active_idx = new as usize;
        }
        RewindPhase::Confirm { active_idx, .. } => {
            let new = (*active_idx as i32 + delta).clamp(0, CONFIRM_OPTIONS as i32 - 1);
            *active_idx = new as usize;
        }
        RewindPhase::FilePreview { active_idx, .. }
        | RewindPhase::OrphanWarning { active_idx, .. } => {
            let new = (*active_idx as i32 + delta).clamp(0, CONFIRM_BACK_OPTIONS as i32 - 1);
            *active_idx = new as usize;
        }
        RewindPhase::ModeSelect {
            active_idx,
            offer_files_only,
            has_file_changes,
            ..
        } => {
            let max = mode_select_count(*offer_files_only) as i32 - 1;
            let mut new = (*active_idx as i32 + delta).clamp(0, max);
            if new == 2 && !*has_file_changes {
                new = 1;
            }
            *active_idx = new as usize;
        }
        _ => {}
    }
}

pub fn confirm_cursor(phase: &RewindPhase) -> RewindInput {
    match phase {
        RewindPhase::CancelOffer { active_idx } => match active_idx {
            0 => RewindInput::CancelTurnThenProceed,
            _ => RewindInput::Dismissed,
        },
        RewindPhase::Confirm {
            target_prompt_index,
            active_idx,
            ..
        } => match active_idx {
            0 => RewindInput::Confirm(*target_prompt_index),
            1 => RewindInput::ConfirmNeverAsk(*target_prompt_index),
            _ => RewindInput::Dismissed,
        },
        RewindPhase::FilePreview {
            target_prompt_index,
            active_idx,
            ..
        }
        | RewindPhase::OrphanWarning {
            target_prompt_index,
            active_idx,
            ..
        } => match active_idx {
            0 => RewindInput::Confirm(*target_prompt_index),
            _ => RewindInput::BackToModeSelect,
        },
        RewindPhase::ModeSelect {
            target_prompt_index,
            has_file_changes,
            active_idx,
            ..
        } => {
            if *active_idx == 2 && !*has_file_changes {
                return RewindInput::Consumed;
            }
            RewindInput::SelectMode(mode_for_idx(*active_idx), *target_prompt_index)
        }
        _ => RewindInput::Consumed,
    }
}

/// Hit-test a screen position against the rewind overlay's clickable rows.
pub fn rewind_row_at(phase: &RewindPhase, area: Rect, col: u16, row: u16) -> Option<usize> {
    if area.height == 0 || area.width < 10 {
        return None;
    }
    if col < area.x || col >= area.x + area.width {
        return None;
    }
    if row < area.y || row >= area.y + area.height {
        return None;
    }
    match phase {
        RewindPhase::Picker { points, selected } => crate::views::overlay_list::ListOverlay {
            len: points.len(),
            selected: *selected,
        }
        .row_at(area, col, row),
        RewindPhase::CancelOffer { .. } => match row.checked_sub(area.y + 3) {
            Some(0) => Some(0),
            Some(1) => Some(1),
            _ => None,
        },
        RewindPhase::Confirm { .. } => match row.checked_sub(area.y + 2) {
            Some(0) => Some(0),
            Some(1) => Some(1),
            Some(2) => Some(2),
            _ => None,
        },
        RewindPhase::FilePreview {
            clean_files,
            conflicts,
            ..
        } => {
            let (rows, gap) = file_preview_rows(clean_files, conflicts);
            match row.checked_sub(area.y + 2 + rows + gap) {
                Some(0) => Some(0),
                Some(1) => Some(1),
                _ => None,
            }
        }
        // Title, then the warning line, then the two rows.
        RewindPhase::OrphanWarning { .. } => match row.checked_sub(area.y + 3) {
            Some(0) => Some(0),
            Some(1) => Some(1),
            _ => None,
        },
        RewindPhase::ModeSelect {
            offer_files_only,
            has_file_changes,
            ..
        } => {
            let n = mode_select_count(*offer_files_only) as u16;
            match row.checked_sub(area.y + 2) {
                // The disabled files-only row is rendered but not clickable.
                Some(2) if !*has_file_changes => None,
                Some(i) if i < n => Some(i as usize),
                _ => None,
            }
        }
        RewindPhase::Error { .. } => {
            if row == area.y + 3 {
                Some(0)
            } else {
                None
            }
        }
        RewindPhase::Loading | RewindPhase::Previewing { .. } | RewindPhase::Executing { .. } => {
            None
        }
    }
}

/// Move the overlay cursor/selection to `idx` (used by mouse hover/click).
/// Returns `true` if the stored cursor changed.
pub fn set_rewind_cursor(phase: &mut RewindPhase, idx: usize) -> bool {
    match phase {
        RewindPhase::Picker { points, selected } => {
            if points.is_empty() {
                return false;
            }
            let new = idx.min(points.len() - 1);
            if *selected != new {
                *selected = new;
                true
            } else {
                false
            }
        }
        RewindPhase::CancelOffer { active_idx } => {
            let new = idx.min(CANCEL_OFFER_OPTIONS - 1);
            if *active_idx != new {
                *active_idx = new;
                true
            } else {
                false
            }
        }
        RewindPhase::Confirm { active_idx, .. } => {
            let new = idx.min(CONFIRM_OPTIONS - 1);
            if *active_idx != new {
                *active_idx = new;
                true
            } else {
                false
            }
        }
        RewindPhase::FilePreview { active_idx, .. }
        | RewindPhase::OrphanWarning { active_idx, .. } => {
            let new = idx.min(CONFIRM_BACK_OPTIONS - 1);
            if *active_idx != new {
                *active_idx = new;
                true
            } else {
                false
            }
        }
        RewindPhase::ModeSelect {
            active_idx,
            offer_files_only,
            has_file_changes,
            ..
        } => {
            let mut new = idx.min(mode_select_count(*offer_files_only) - 1);
            if new == 2 && !*has_file_changes {
                new = 1;
            }
            if *active_idx != new {
                *active_idx = new;
                true
            } else {
                false
            }
        }
        _ => false,
    }
}

/// The activation input for the current cursor position, equivalent to pressing Enter on the focused row. Used by mouse-click handling.
pub fn rewind_activate(phase: &RewindPhase) -> RewindInput {
    match phase {
        RewindPhase::Picker { points, selected } => points
            .get(*selected)
            .map(|p| RewindInput::PickerSelect(p.prompt_index))
            .unwrap_or(RewindInput::Consumed),
        RewindPhase::Error { .. } => RewindInput::DismissError,
        other => confirm_cursor(other),
    }
}

pub fn rewind_overlay_height(phase: &RewindPhase, screen_h: u16) -> u16 {
    let content = match phase {
        RewindPhase::Loading => 2,
        RewindPhase::Picker { points, selected } => {
            return crate::views::overlay_list::ListOverlay {
                len: points.len(),
                selected: *selected,
            }
            .height(screen_h);
        }
        RewindPhase::CancelOffer { .. } => 5,
        RewindPhase::Previewing { .. } | RewindPhase::Executing { .. } => 2,
        RewindPhase::Confirm { .. } => 5,
        RewindPhase::OrphanWarning { .. } => 5,
        RewindPhase::FilePreview {
            clean_files,
            conflicts,
            ..
        } => {
            let (rows, gap) = file_preview_rows(clean_files, conflicts);
            4 + rows + gap
        }
        // Title, then one row per mode.
        RewindPhase::ModeSelect {
            offer_files_only, ..
        } => 2 + mode_select_count(*offer_files_only) as u16,
        RewindPhase::Error { .. } => 4,
    };
    content + 1
}

/// Fit `preview` between `prefix` and `suffix` inside `content_w`, ellipsizing it when needed.
fn preview_title(prefix: &str, preview: &str, suffix: &str, content_w: u16) -> String {
    let chrome = prefix.chars().count() + suffix.chars().count();
    let max_preview = (content_w as usize).saturating_sub(chrome + 1);
    let trimmed: String = if preview.chars().count() > max_preview {
        let truncated: String = preview
            .chars()
            .take(max_preview.saturating_sub(1))
            .collect();
        format!("{truncated}\u{2026}")
    } else {
        preview.to_string()
    };
    format!("{prefix}{trimmed}{suffix}")
}

pub fn render_rewind_overlay(buf: &mut Buffer, area: Rect, phase: &RewindPhase, focused: bool) {
    if area.height == 0 || area.width < 10 {
        return;
    }

    let theme = Theme::current();
    let bg = theme.bg_light;

    buf.set_style(area, Style::default().bg(bg));

    let accent_style = Style::default().fg(theme.accent_user);
    for row in area.y..area.y + area.height {
        if let Some(cell) = buf.cell_mut((area.x, row)) {
            cell.set_symbol(crate::glyphs::accent_bar());
            cell.set_style(accent_style);
        }
    }

    let content_x = area.x + 3;
    let content_w = area.width.saturating_sub(5);

    let title_style = Style::default()
        .fg(theme.accent_user)
        .add_modifier(Modifier::BOLD);

    match phase {
        RewindPhase::Loading => {
            let y = area.y + 1;
            buf.set_line(
                content_x,
                y,
                &Line::from(Span::styled(
                    "Loading rewind points...",
                    Style::default().fg(theme.gray),
                )),
                content_w,
            );
        }
        RewindPhase::Picker { points, selected } => {
            // Shared list-overlay frame and row geometry (also used by /jump)
            // It applies the unfocus dim itself, so return before the shared blend at the bottom of this function
            crate::views::overlay_list::ListOverlay {
                len: points.len(),
                selected: *selected,
            }
            .render(buf, area, "Rewind to which turn?", focused, |i, ctx| {
                let point = &points[i];
                let dot_style = Style::default().fg(theme.gray).bg(ctx.row_bg);
                let preview: String = crate::render::line_utils::truncate_str(
                    point.prompt_preview.as_deref().unwrap_or("(no preview)"),
                    ctx.content_width.saturating_sub(8) as usize,
                );
                let text_style = Style::default()
                    .fg(theme.text_primary)
                    .bg(ctx.row_bg)
                    .add_modifier(if ctx.is_cursor {
                        Modifier::BOLD
                    } else {
                        Modifier::empty()
                    });
                let meta_style = Style::default().fg(theme.gray).bg(ctx.row_bg);

                let file_info = if point.num_file_snapshots > 0 {
                    format!(" \u{00B7} {}", file_count_phrase(point.num_file_snapshots))
                } else {
                    String::new()
                };

                Line::from(vec![
                    Span::styled("\u{00B7} ", dot_style),
                    Span::styled(preview, text_style),
                    Span::styled(file_info, meta_style),
                ])
            });
            return;
        }
        RewindPhase::CancelOffer { active_idx } => {
            let mut y = area.y + 1;
            buf.set_line(
                content_x,
                y,
                &Line::from(Span::styled("A turn is currently running.", title_style)),
                content_w,
            );
            y += 1;
            buf.set_line(
                content_x,
                y,
                &Line::from(Span::styled(
                    "Would you like to cancel it before rewinding?",
                    Style::default().fg(theme.gray),
                )),
                content_w,
            );
            y += 1;
            render_radio_row(
                buf,
                content_x,
                y,
                content_w,
                'y',
                "Cancel turn and rewind",
                true,
                *active_idx == 0,
                focused,
                &theme,
            );
            y += 1;
            render_radio_row(
                buf,
                content_x,
                y,
                content_w,
                'n',
                "Let it finish",
                true,
                *active_idx == 1,
                focused,
                &theme,
            );
        }
        RewindPhase::Previewing { .. } => {
            let y = area.y + 1;
            buf.set_line(
                content_x,
                y,
                &Line::from(Span::styled(
                    "Previewing file changes...",
                    Style::default().fg(theme.gray),
                )),
                content_w,
            );
        }
        RewindPhase::Executing { .. } => {
            let y = area.y + 1;
            buf.set_line(
                content_x,
                y,
                &Line::from(Span::styled(
                    "Rewinding...",
                    Style::default().fg(theme.gray),
                )),
                content_w,
            );
        }
        RewindPhase::ModeSelect {
            has_file_changes,
            offer_files_only,
            active_idx,
            ..
        } => {
            let mut y = area.y + 1;
            // Inline edit-and-resubmit: the conversation rewind is a given, so the only question is whether files come along
            let title = if *offer_files_only {
                "What do you want to rewind?"
            } else {
                "Resubmit from here \u{2014} what should be rewound?"
            };
            buf.set_line(
                content_x,
                y,
                &Line::from(Span::styled(title, title_style)),
                content_w,
            );
            y += 1;
            render_radio_row(
                buf,
                content_x,
                y,
                content_w,
                'a',
                "Both conversation and file changes",
                true,
                *active_idx == 0,
                focused,
                &theme,
            );
            y += 1;
            render_radio_row(
                buf,
                content_x,
                y,
                content_w,
                // Sequential lettering in the two-row inline variant; the mnemonic 'c' only reads right with the 'f' row present
                if *offer_files_only { 'c' } else { 'b' },
                "Conversation only",
                true,
                *active_idx == 1,
                focused,
                &theme,
            );
            if *offer_files_only {
                y += 1;
                let files_label = if *has_file_changes {
                    "File changes only"
                } else {
                    "File changes only (none since this turn)"
                };
                render_radio_row(
                    buf,
                    content_x,
                    y,
                    content_w,
                    'f',
                    files_label,
                    *has_file_changes,
                    *active_idx == 2,
                    focused,
                    &theme,
                );
            }
        }
        RewindPhase::FilePreview {
            clean_files,
            conflicts,
            mode,
            active_idx,
            prompt_preview,
            ..
        } => {
            let mut y = area.y + 1;
            let file_total = clean_files.len() + conflicts.len();
            let preview_text = prompt_preview.as_deref().unwrap_or("this turn");
            let prefix = match mode {
                RewindMode::FilesOnly => "Rewind file changes only to \u{201C}",
                _ => "Rewind file changes and conversation to \u{201C}",
            };
            let suffix = if file_total > 0 {
                format!("\u{201D}? ({})", file_count_phrase(file_total))
            } else {
                "\u{201D}?".to_string()
            };
            buf.set_line(
                content_x,
                y,
                &Line::from(Span::styled(
                    preview_title(prefix, preview_text, &suffix, content_w),
                    title_style,
                )),
                content_w,
            );
            y += 1;

            if clean_files.is_empty() && conflicts.is_empty() {
                buf.set_line(
                    content_x,
                    y,
                    &Line::from(Span::styled(
                        "No tracked file changes to restore.",
                        Style::default().fg(theme.gray),
                    )),
                    content_w,
                );
                y += 1;
            } else {
                for (i, path) in clean_files.iter().enumerate() {
                    if i >= 5 {
                        let more = format!("+{} more", clean_files.len() - 5);
                        buf.set_line(
                            content_x,
                            y,
                            &Line::from(Span::styled(more, Style::default().fg(theme.gray))),
                            content_w,
                        );
                        y += 1;
                        break;
                    }
                    buf.set_line(
                        content_x,
                        y,
                        &Line::from(Span::styled(
                            path.to_string(),
                            Style::default().fg(theme.gray),
                        )),
                        content_w,
                    );
                    y += 1;
                }
                for (i, conflict) in conflicts.iter().enumerate() {
                    if i >= 5 {
                        let more = format!("+{} more", conflicts.len() - 5);
                        buf.set_line(
                            content_x,
                            y,
                            &Line::from(Span::styled(more, Style::default().fg(theme.gray))),
                            content_w,
                        );
                        y += 1;
                        break;
                    }
                    let line_text = format!("! {} ({})", conflict.path, conflict.label);
                    buf.set_line(
                        content_x,
                        y,
                        &Line::from(Span::styled(line_text, Style::default().fg(theme.warning))),
                        content_w,
                    );
                    y += 1;
                }
                y += 1;
            }

            render_radio_row(
                buf,
                content_x,
                y,
                content_w,
                'y',
                "Confirm rewind",
                true,
                *active_idx == 0,
                focused,
                &theme,
            );
            y += 1;
            render_radio_row(
                buf,
                content_x,
                y,
                content_w,
                '\x08',
                "Back",
                true,
                *active_idx == 1,
                focused,
                &theme,
            );
        }
        RewindPhase::OrphanWarning {
            active_idx,
            prompt_preview,
            ..
        } => {
            let mut y = area.y + 1;
            let preview_text = prompt_preview.as_deref().unwrap_or("this turn");
            buf.set_line(
                content_x,
                y,
                &Line::from(Span::styled(
                    preview_title(
                        "Rewind conversation only to \u{201C}",
                        preview_text,
                        "\u{201D}?",
                        content_w,
                    ),
                    title_style,
                )),
                content_w,
            );
            y += 1;
            buf.set_line(
                content_x,
                y,
                &Line::from(Span::styled(
                    "The removed turns' file changes stay on disk and cannot be undone later.",
                    Style::default().fg(theme.warning),
                )),
                content_w,
            );
            y += 1;
            render_radio_row(
                buf,
                content_x,
                y,
                content_w,
                'y',
                "Confirm rewind",
                true,
                *active_idx == 0,
                focused,
                &theme,
            );
            y += 1;
            render_radio_row(
                buf,
                content_x,
                y,
                content_w,
                '\x08',
                "Back",
                true,
                *active_idx == 1,
                focused,
                &theme,
            );
        }
        RewindPhase::Confirm {
            active_idx,
            prompt_preview,
            mode,
            ..
        } => {
            let mut y = area.y + 1;
            let preview_text = prompt_preview.as_deref().unwrap_or("this turn");
            // Reached only when nothing moves on disk (anything that touches files goes
            // through FilePreview instead), so `All` and `ConversationOnly` read the same.
            let prefix = match mode {
                RewindMode::All | RewindMode::ConversationOnly => "Rewind conversation to \u{201C}",
                RewindMode::FilesOnly => "Rewind files to \u{201C}",
            };
            buf.set_line(
                content_x,
                y,
                &Line::from(Span::styled(
                    preview_title(prefix, preview_text, "\u{201D}?", content_w),
                    title_style,
                )),
                content_w,
            );
            y += 1;
            render_radio_row(
                buf,
                content_x,
                y,
                content_w,
                'y',
                "Yes",
                true,
                *active_idx == 0,
                focused,
                &theme,
            );
            y += 1;
            render_radio_row(
                buf,
                content_x,
                y,
                content_w,
                'a',
                "Yes, and don't ask again",
                true,
                *active_idx == 1,
                focused,
                &theme,
            );
            y += 1;
            render_radio_row(
                buf,
                content_x,
                y,
                content_w,
                'n',
                "No",
                true,
                *active_idx == 2,
                focused,
                &theme,
            );
        }
        RewindPhase::Error { message } => {
            let mut y = area.y + 1;
            buf.set_line(
                content_x,
                y,
                &Line::from(Span::styled(
                    "Rewind failed",
                    Style::default()
                        .fg(theme.accent_error)
                        .add_modifier(Modifier::BOLD),
                )),
                content_w,
            );
            y += 1;
            let truncated: String = message.chars().take(content_w as usize).collect();
            buf.set_line(
                content_x,
                y,
                &Line::from(Span::styled(
                    truncated,
                    Style::default().fg(theme.text_primary),
                )),
                content_w,
            );
            y += 1;
            render_radio_row(
                buf, content_x, y, content_w, '\x1b', "Dismiss", true, true, focused, &theme,
            );
        }
    }

    // Unfocus dim: when the prompt area is unfocused (user moved to scrollback), blend foregrounds toward `bg_light` so the panel recedes
    // Mirrors the unfocused prompt widget pattern (see `prompt_widget.rs`)
    if !focused {
        crate::render::color::recede_area(buf, area, bg, 0.66);
    }
}

/// Visible label for sentinel-encoded keys (`Esc`, `Bksp`).
fn key_label(key: char) -> String {
    match key {
        '\x1b' => "Esc".into(),
        '\x08' => "Bksp".into(),
        other => other.to_string(),
    }
}

#[allow(clippy::too_many_arguments)]
fn render_radio_row(
    buf: &mut Buffer,
    x: u16,
    y: u16,
    w: u16,
    key: char,
    label: &str,
    enabled: bool,
    is_cursor: bool,
    panel_focused: bool,
    theme: &Theme,
) {
    let bg = theme.bg_light;

    let row_rect = Rect {
        x: x.saturating_sub(1),
        y,
        width: w + 2,
        height: 1,
    };
    buf.set_style(row_rect, Style::default().bg(bg));

    let key_display = key_label(key);

    if !enabled {
        let dim_style = Style::default().fg(theme.gray_dim).bg(bg);
        let line = Line::from(vec![
            Span::styled(format!("{key_display:<4}"), dim_style),
            Span::styled("(\u{25CB}) ", dim_style),
            Span::styled(label.to_string(), dim_style),
        ]);
        buf.set_line(x, y, &line, w);
        return;
    }

    let marker = if is_cursor {
        crate::glyphs::filled_dot()
    } else {
        "\u{25CB}"
    };

    let num_style = Style::default().fg(theme.accent_user).bg(bg);
    let marker_style = if is_cursor {
        Style::default().fg(theme.accent_user).bg(bg)
    } else {
        Style::default().fg(theme.gray).bg(bg)
    };
    let label_style = Style::default()
        .fg(theme.text_primary)
        .bg(bg)
        .add_modifier(if is_cursor {
            Modifier::BOLD
        } else {
            Modifier::empty()
        });

    let line = Line::from(vec![
        Span::styled(format!("{key_display:<4}"), num_style),
        Span::styled(format!("({marker}) "), marker_style),
        Span::styled(label.to_string(), label_style),
    ]);
    buf.set_line(x, y, &line, w);
    if is_cursor && panel_focused {
        buf.set_style(row_rect, theme.selection_overlay());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::{KeyEventKind, KeyModifiers};

    fn area() -> Rect {
        Rect {
            x: 0,
            y: 0,
            width: 40,
            height: 10,
        }
    }

    fn point(prompt_index: usize) -> RewindPointInfo {
        RewindPointInfo {
            prompt_index,
            created_at: String::new(),
            num_file_snapshots: 0,
            prompt_preview: Some(format!("turn {prompt_index}")),
            has_file_changes: false,
        }
    }

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent {
            code,
            modifiers: KeyModifiers::empty(),
            kind: KeyEventKind::Press,
            state: crossterm::event::KeyEventState::empty(),
        }
    }

    fn confirm_phase(target: usize, active_idx: usize) -> RewindPhase {
        RewindPhase::Confirm {
            target_prompt_index: target,
            active_idx,
            prompt_preview: None,
            mode: RewindMode::ConversationOnly,
        }
    }

    fn confirm_state() -> RewindState {
        RewindState {
            phase: confirm_phase(3, 0),
            anchor_entry_idx: 0,
            stashed_draft: None,
            selected_prompt_index: Some(3),
        }
    }

    fn file_preview_phase(active_idx: usize) -> RewindPhase {
        RewindPhase::FilePreview {
            target_prompt_index: 2,
            mode: RewindMode::All,
            clean_files: vec!["a.rs".into(), "b.rs".into()],
            conflicts: vec![ConflictDisplay {
                path: "c.rs".into(),
                label: "modified",
            }],
            active_idx,
            prompt_preview: Some("turn 2".into()),
        }
    }

    fn state_with(phase: RewindPhase) -> RewindState {
        RewindState {
            phase,
            anchor_entry_idx: 0,
            stashed_draft: None,
            selected_prompt_index: None,
        }
    }

    #[test]
    fn picker_row_hit_test_maps_to_point_index() {
        let phase = RewindPhase::Picker {
            points: vec![point(0), point(1), point(2)],
            selected: 0,
        };
        // Title is at y+1; rows start at y+2.
        assert_eq!(rewind_row_at(&phase, area(), 5, 1), None);
        assert_eq!(rewind_row_at(&phase, area(), 5, 2), Some(0));
        assert_eq!(rewind_row_at(&phase, area(), 5, 4), Some(2));
        // Past the last point.
        assert_eq!(rewind_row_at(&phase, area(), 5, 5), None);
        // Outside the overlay horizontally.
        assert_eq!(rewind_row_at(&phase, area(), 99, 2), None);
    }

    #[test]
    fn cancel_offer_rows() {
        let phase = RewindPhase::CancelOffer { active_idx: 0 };
        assert_eq!(rewind_row_at(&phase, area(), 5, 3), Some(0));
        assert_eq!(rewind_row_at(&phase, area(), 5, 4), Some(1));
        assert_eq!(rewind_row_at(&phase, area(), 5, 5), None);
    }

    #[test]
    fn confirm_rows() {
        let phase = confirm_phase(0, 0);
        assert_eq!(rewind_row_at(&phase, area(), 5, 2), Some(0));
        assert_eq!(rewind_row_at(&phase, area(), 5, 3), Some(1));
        assert_eq!(rewind_row_at(&phase, area(), 5, 4), Some(2));
        assert_eq!(rewind_row_at(&phase, area(), 5, 5), None);
    }

    /// Every phase reserves a leading blank row and a trailing one, so the height is
    /// (rows the render loop writes) + 1. A short overlay clips the last radio.
    #[test]
    fn overlay_height_covers_every_rendered_row() {
        let with_files = RewindPhase::ModeSelect {
            target_prompt_index: 1,
            has_file_changes: true,
            offer_files_only: true,
            active_idx: 0,
            prompt_preview: None,
        };
        // Blank + title + 3 radios + trailing blank.
        assert_eq!(rewind_overlay_height(&with_files, 40), 6);

        let RewindPhase::ModeSelect {
            target_prompt_index,
            has_file_changes,
            active_idx,
            ..
        } = with_files
        else {
            unreachable!()
        };
        let inline = RewindPhase::ModeSelect {
            target_prompt_index,
            has_file_changes,
            offer_files_only: false,
            active_idx,
            prompt_preview: None,
        };
        assert_eq!(rewind_overlay_height(&inline, 40), 5);

        // Blank + title + 2 clean + 1 conflict + spacer + 2 radios + trailing blank.
        assert_eq!(rewind_overlay_height(&file_preview_phase(0), 40), 9);
    }

    /// FilePreview radios sit below the file list plus a one-row spacer.
    #[test]
    fn file_preview_rows_track_the_file_list() {
        // Title(1) + 2 clean + 1 conflict + 1 spacer -> radios at y+6/y+7.
        let phase = file_preview_phase(0);
        assert_eq!(rewind_row_at(&phase, area(), 5, 5), None);
        assert_eq!(rewind_row_at(&phase, area(), 5, 6), Some(0));
        assert_eq!(rewind_row_at(&phase, area(), 5, 7), Some(1));
        assert_eq!(rewind_row_at(&phase, area(), 5, 8), None);

        // Empty lists render one "nothing to restore" line and no spacer.
        let empty = RewindPhase::FilePreview {
            target_prompt_index: 2,
            mode: RewindMode::FilesOnly,
            clean_files: vec![],
            conflicts: vec![],
            active_idx: 0,
            prompt_preview: None,
        };
        assert_eq!(rewind_row_at(&empty, area(), 5, 3), Some(0));
        assert_eq!(rewind_row_at(&empty, area(), 5, 4), Some(1));
    }

    #[test]
    fn orphan_warning_rows() {
        let phase = RewindPhase::OrphanWarning {
            target_prompt_index: 0,
            active_idx: 0,
            prompt_preview: None,
        };
        assert_eq!(rewind_row_at(&phase, area(), 5, 2), None);
        assert_eq!(rewind_row_at(&phase, area(), 5, 3), Some(0));
        assert_eq!(rewind_row_at(&phase, area(), 5, 4), Some(1));
        assert_eq!(rewind_row_at(&phase, area(), 5, 5), None);
    }

    #[test]
    fn error_dismiss_row() {
        let phase = RewindPhase::Error {
            message: "boom".into(),
        };
        assert_eq!(rewind_row_at(&phase, area(), 5, 3), Some(0));
        assert_eq!(rewind_row_at(&phase, area(), 5, 2), None);
    }

    #[test]
    fn non_interactive_phases_have_no_rows() {
        for phase in [
            RewindPhase::Loading,
            RewindPhase::Previewing {
                target_prompt_index: 0,
                mode: RewindMode::All,
            },
            RewindPhase::Executing {
                target_prompt_index: 0,
                mode: RewindMode::All,
            },
        ] {
            for row in 0..10 {
                assert_eq!(rewind_row_at(&phase, area(), 5, row), None);
            }
        }
    }

    #[test]
    fn set_cursor_moves_and_clamps() {
        let mut phase = RewindPhase::Picker {
            points: vec![point(0), point(1)],
            selected: 0,
        };
        assert!(set_rewind_cursor(&mut phase, 1));
        assert!(!set_rewind_cursor(&mut phase, 1)); // no change
        // Clamp out-of-range to last point (already at last, so no change)
        assert!(!set_rewind_cursor(&mut phase, 99));
        if let RewindPhase::Picker { selected, .. } = phase {
            assert_eq!(selected, 1);
        } else {
            panic!("expected picker");
        }

        let mut confirm = confirm_phase(0, 0);
        set_rewind_cursor(&mut confirm, 2);
        if let RewindPhase::Confirm { active_idx, .. } = confirm {
            assert_eq!(active_idx, 2);
        } else {
            panic!("expected confirm");
        }
        set_rewind_cursor(&mut confirm, 99);
        if let RewindPhase::Confirm { active_idx, .. } = confirm {
            assert_eq!(active_idx, 2);
        } else {
            panic!("expected confirm");
        }

        let mut preview = file_preview_phase(0);
        set_rewind_cursor(&mut preview, 99);
        if let RewindPhase::FilePreview { active_idx, .. } = preview {
            assert_eq!(active_idx, 1, "clamped to the Back row");
        } else {
            panic!("expected file preview");
        }
    }

    #[test]
    fn activate_matches_enter_semantics() {
        let picker = RewindPhase::Picker {
            points: vec![point(10), point(20)],
            selected: 1,
        };
        assert!(matches!(
            rewind_activate(&picker),
            RewindInput::PickerSelect(20)
        ));

        let error = RewindPhase::Error {
            message: "x".into(),
        };
        assert!(matches!(rewind_activate(&error), RewindInput::DismissError));

        let confirm_go = confirm_phase(4, 0);
        assert!(matches!(
            rewind_activate(&confirm_go),
            RewindInput::Confirm(4)
        ));

        let confirm_never = confirm_phase(4, 1);
        assert!(matches!(
            rewind_activate(&confirm_never),
            RewindInput::ConfirmNeverAsk(4)
        ));

        let confirm_no = confirm_phase(4, 2);
        assert!(matches!(
            rewind_activate(&confirm_no),
            RewindInput::Dismissed
        ));

        assert!(matches!(
            rewind_activate(&file_preview_phase(0)),
            RewindInput::Confirm(2)
        ));
        assert!(matches!(
            rewind_activate(&file_preview_phase(1)),
            RewindInput::BackToModeSelect
        ));
    }

    #[test]
    fn confirm_letter_keys() {
        let state = confirm_state();
        assert!(matches!(
            handle_rewind_key(&state, &key(KeyCode::Char('y'))),
            RewindInput::Confirm(3)
        ));
        assert!(matches!(
            handle_rewind_key(&state, &key(KeyCode::Char('n'))),
            RewindInput::Dismissed
        ));
        assert!(matches!(
            handle_rewind_key(&state, &key(KeyCode::Char('a'))),
            RewindInput::ConfirmNeverAsk(3)
        ));
    }

    #[test]
    fn esc_dismisses_from_confirm() {
        let state = confirm_state();
        assert!(matches!(
            handle_rewind_key(&state, &key(KeyCode::Esc)),
            RewindInput::Dismissed
        ));
    }

    #[test]
    fn backspace_from_confirm_returns_to_mode_select() {
        let state = confirm_state();
        assert!(matches!(
            handle_rewind_key(&state, &key(KeyCode::Backspace)),
            RewindInput::BackToModeSelect
        ));
    }

    /// y confirms, Backspace goes back, Esc leaves the flow entirely.
    #[test]
    fn file_preview_keys() {
        let state = state_with(file_preview_phase(0));
        assert!(matches!(
            handle_rewind_key(&state, &key(KeyCode::Char('y'))),
            RewindInput::Confirm(2)
        ));
        assert!(matches!(
            handle_rewind_key(&state, &key(KeyCode::Backspace)),
            RewindInput::BackToModeSelect
        ));
        assert!(matches!(
            handle_rewind_key(&state, &key(KeyCode::Esc)),
            RewindInput::Dismissed
        ));
    }

    #[test]
    fn orphan_warning_keys() {
        let state = state_with(RewindPhase::OrphanWarning {
            target_prompt_index: 0,
            active_idx: 0,
            prompt_preview: None,
        });
        assert!(matches!(
            handle_rewind_key(&state, &key(KeyCode::Char('y'))),
            RewindInput::Confirm(0)
        ));
        assert!(matches!(
            handle_rewind_key(&state, &key(KeyCode::Backspace)),
            RewindInput::BackToModeSelect
        ));
        assert!(matches!(
            handle_rewind_key(&state, &key(KeyCode::Esc)),
            RewindInput::Dismissed
        ));
        assert!(matches!(
            handle_rewind_key(&state, &key(KeyCode::Enter)),
            RewindInput::ConfirmCursor
        ));
    }

    #[test]
    fn mode_select_letter_keys() {
        let state = state_with(RewindPhase::ModeSelect {
            target_prompt_index: 2,
            has_file_changes: true,
            offer_files_only: true,
            active_idx: 0,
            prompt_preview: None,
        });
        assert!(matches!(
            handle_rewind_key(&state, &key(KeyCode::Char('a'))),
            RewindInput::SelectMode(RewindMode::All, 2)
        ));
        assert!(matches!(
            handle_rewind_key(&state, &key(KeyCode::Char('c'))),
            RewindInput::SelectMode(RewindMode::ConversationOnly, 2)
        ));
        assert!(matches!(
            handle_rewind_key(&state, &key(KeyCode::Char('f'))),
            RewindInput::SelectMode(RewindMode::FilesOnly, 2)
        ));
    }

    #[test]
    fn files_only_hidden_when_offer_files_only_is_false() {
        let phase = RewindPhase::ModeSelect {
            target_prompt_index: 1,
            has_file_changes: true,
            offer_files_only: false,
            active_idx: 0,
            prompt_preview: None,
        };
        assert_eq!(rewind_row_at(&phase, area(), 5, 2), Some(0));
        assert_eq!(rewind_row_at(&phase, area(), 5, 3), Some(1));
        assert_eq!(rewind_row_at(&phase, area(), 5, 4), None);
    }

    /// Without tracked edits the files-only row is rendered dim: the mouse cannot hit it,
    /// the cursor cannot land on it, and `f` does nothing.
    #[test]
    fn disabled_files_only_row_is_not_reachable() {
        let phase = RewindPhase::ModeSelect {
            target_prompt_index: 1,
            has_file_changes: false,
            offer_files_only: true,
            active_idx: 0,
            prompt_preview: None,
        };
        assert_eq!(rewind_row_at(&phase, area(), 5, 2), Some(0));
        assert_eq!(rewind_row_at(&phase, area(), 5, 3), Some(1));
        assert_eq!(rewind_row_at(&phase, area(), 5, 4), None, "row disabled");

        let mut cursor = phase.clone();
        move_cursor(&mut cursor, 1);
        move_cursor(&mut cursor, 1);
        move_cursor(&mut cursor, 1);
        if let RewindPhase::ModeSelect { active_idx, .. } = cursor {
            assert_eq!(active_idx, 1, "keyboard clamps below the disabled row");
        } else {
            panic!("expected mode select");
        }
        set_rewind_cursor(&mut cursor, 2);
        if let RewindPhase::ModeSelect { active_idx, .. } = cursor {
            assert_eq!(active_idx, 1, "mouse clamps below the disabled row");
        } else {
            panic!("expected mode select");
        }

        assert!(matches!(
            handle_rewind_key(&state_with(phase), &key(KeyCode::Char('f'))),
            RewindInput::Consumed
        ));
    }

    #[test]
    fn esc_dismisses_from_picker_and_other_phases() {
        let s = state_with(RewindPhase::Picker {
            points: vec![],
            selected: 0,
        });
        assert!(matches!(
            handle_rewind_key(&s, &key(KeyCode::Esc)),
            RewindInput::Dismissed
        ));

        let s = RewindState::new_cancel_offer(0, None, None);
        assert!(matches!(
            handle_rewind_key(&s, &key(KeyCode::Esc)),
            RewindInput::Dismissed
        ));

        let s = state_with(RewindPhase::Loading);
        assert!(matches!(
            handle_rewind_key(&s, &key(KeyCode::Esc)),
            RewindInput::Dismissed
        ));

        let s = state_with(RewindPhase::Previewing {
            target_prompt_index: 1,
            mode: RewindMode::All,
        });
        assert!(matches!(
            handle_rewind_key(&s, &key(KeyCode::Esc)),
            RewindInput::Dismissed
        ));
    }

    #[test]
    fn conflict_display_labels() {
        let label = |t: &str| {
            ConflictDisplay::from_conflict(&RewindConflictInfo {
                path: "x".into(),
                conflict_type: t.into(),
            })
            .label
        };
        assert_eq!(label("modified_externally"), "modified");
        assert_eq!(label("deleted_externally"), "deleted");
        assert_eq!(label("created_externally"), "added");
        assert_eq!(label("something_else"), "conflict");
    }

    #[test]
    fn file_count_phrase_is_singular_for_one() {
        assert_eq!(file_count_phrase(1), "1 file");
        assert_eq!(file_count_phrase(0), "0 files");
        assert_eq!(file_count_phrase(3), "3 files");
    }

    #[test]
    fn key_label_renders_special_sentinels() {
        assert_eq!(key_label('\x1b'), "Esc");
        assert_eq!(key_label('\x08'), "Bksp");
        assert_eq!(key_label('y'), "y");
        assert_eq!(key_label('a'), "a");
    }
}
