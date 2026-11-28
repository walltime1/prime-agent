//! Inline tool-approval panel: mounted in the editor dock when the worker
//! requests user confirmation before running a `python_repl` cell.

use pa_types::sandbox::{ToolApprovalDecision, ToolApprovalKind, ToolApprovalRequest};

use crate::keybindings::KeybindingsManager;
use crate::menu_panel::{hint_row, key_hint, menu_row, rule_row, MenuSegment};
use crate::theme::{Theme, ThemeColor};
use crate::width::truncate_line;
use crate::{Line, Span};

/// Maximum preview lines shown for the pending command/code snippet.
const MAX_PREVIEW_LINES: usize = 6;

/// Outcome of one key press while the approval panel owns the dock.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ApprovalAction {
    /// User chose an approval decision (or denied via Esc / Ctrl+C / `n`).
    Resolve(ToolApprovalDecision),
    /// Navigation or unhandled key; panel stays open.
    None,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ApprovalOption {
    label: String,
    hotkey: char,
    decision: ToolApprovalDecision,
}

/// Inline tool-approval panel owning the editor dock while open.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApprovalPanel {
    request: ToolApprovalRequest,
    options: Vec<ApprovalOption>,
    selected: usize,
}

impl ApprovalPanel {
    /// Build a panel for one [`ToolApprovalRequest`].
    #[must_use]
    pub fn new(request: ToolApprovalRequest) -> Self {
        let mut options = vec![ApprovalOption {
            label: "Allow once".to_string(),
            hotkey: 'y',
            decision: ToolApprovalDecision::AllowOnce,
        }];
        match request.kind {
            ToolApprovalKind::FileEdit => {
                options.push(ApprovalOption {
                    label: "Allow edits for session".to_string(),
                    hotkey: 's',
                    decision: ToolApprovalDecision::AllowSessionEdits,
                });
            }
            ToolApprovalKind::BashCommand => {
                if request.risk_reason.is_none() {
                    if let Some(prefix) = &request.allow_prefix {
                        options.push(ApprovalOption {
                            label: format!("Allow \"{prefix}\" for session"),
                            hotkey: 's',
                            decision: ToolApprovalDecision::AllowSessionPrefix {
                                prefix: prefix.clone(),
                            },
                        });
                    }
                }
            }
            ToolApprovalKind::RiskyBashCommand | ToolApprovalKind::OpaquePython => {}
        }
        options.push(ApprovalOption {
            label: "Switch to YOLO mode".to_string(),
            hotkey: 'a',
            decision: ToolApprovalDecision::SwitchToYolo,
        });
        options.push(ApprovalOption {
            label: "Deny".to_string(),
            hotkey: 'n',
            decision: ToolApprovalDecision::Deny { reason: None },
        });
        Self {
            request,
            options,
            selected: 0,
        }
    }

    /// The wire request id this panel resolves.
    #[must_use]
    pub fn request_id(&self) -> &str {
        &self.request.request_id
    }

    /// Handle one key id: arrows navigate, Enter selects, hotkeys (`y`, `s`, `a`, `n`)
    /// resolve directly, and Esc / Ctrl+C deny.
    pub fn handle_key(&mut self, kb: &KeybindingsManager, id: &str) -> ApprovalAction {
        if id == "ctrl+c" || kb.matches(id, "tui.select.cancel") {
            return ApprovalAction::Resolve(ToolApprovalDecision::Deny { reason: None });
        }
        if kb.matches(id, "tui.select.up") {
            self.selected = self.selected.saturating_sub(1);
            return ApprovalAction::None;
        }
        if kb.matches(id, "tui.select.down") {
            self.selected = (self.selected + 1).min(self.options.len().saturating_sub(1));
            return ApprovalAction::None;
        }
        if kb.matches(id, "tui.select.confirm") {
            if let Some(option) = self.options.get(self.selected) {
                return ApprovalAction::Resolve(option.decision.clone());
            }
            return ApprovalAction::None;
        }
        if let Some(ch) = single_ascii_lower(id) {
            if let Some(option) = self.options.iter().find(|option| option.hotkey == ch) {
                return ApprovalAction::Resolve(option.decision.clone());
            }
        }
        ApprovalAction::None
    }

    /// Render the panel rows for the dock.
    #[must_use]
    pub fn render(&self, theme: &Theme, width: usize, kb: &KeybindingsManager) -> Vec<Line> {
        let mut lines: Vec<Line> = Vec::new();
        lines.push(rule_row(theme, width));
        lines.push(truncate_line(
            &vec![
                Span::raw("  "),
                theme.bold(theme.fg_span(ThemeColor::Accent, self.request.summary.clone())),
            ],
            width,
            "\u{2026}",
        ));
        if let Some(risk) = self
            .request
            .risk_reason
            .as_deref()
            .filter(|reason| !reason.trim().is_empty())
        {
            lines.push(truncate_line(
                &vec![
                    Span::raw("  "),
                    theme.fg_span(ThemeColor::Warning, format!("\u{26a0} {risk}")),
                ],
                width,
                "\u{2026}",
            ));
        }
        let preview_lines: Vec<&str> = self.request.detail.lines().collect();
        if !preview_lines.is_empty() {
            lines.push(Vec::new());
            let shown = preview_lines.len().min(MAX_PREVIEW_LINES);
            for preview in &preview_lines[..shown] {
                lines.push(truncate_line(
                    &vec![theme.fg_span(ThemeColor::Muted, format!("    {preview}"))],
                    width,
                    "\u{2026}",
                ));
            }
            if preview_lines.len() > MAX_PREVIEW_LINES {
                let extra = preview_lines.len() - MAX_PREVIEW_LINES;
                lines.push(truncate_line(
                    &vec![theme.fg_span(ThemeColor::Dim, format!("    \u{2026} (+{extra} more)"))],
                    width,
                    "",
                ));
            }
        }
        lines.push(Vec::new());
        for (index, option) in self.options.iter().enumerate() {
            let shortcut = if option.hotkey == 'n' {
                "n / Esc".to_string()
            } else {
                option.hotkey.to_string()
            };
            lines.push(menu_row(
                theme,
                width,
                vec![Span::raw(option.label.clone())],
                &[MenuSegment::themed(ThemeColor::Dim, &shortcut)],
                index == self.selected,
            ));
        }
        lines.push(Vec::new());
        lines.push(hint_row(theme, width, &hint(kb)));
        lines
    }
}

fn single_ascii_lower(id: &str) -> Option<char> {
    let mut chars = id.chars();
    let ch = chars.next()?;
    if chars.next().is_some() || !ch.is_ascii_alphabetic() {
        return None;
    }
    Some(ch.to_ascii_lowercase())
}

fn hint(kb: &KeybindingsManager) -> String {
    [
        key_hint(kb, &["tui.select.up", "tui.select.down"], "navigate"),
        key_hint(kb, &["tui.select.confirm"], "select"),
        key_hint(kb, &["tui.select.cancel"], "deny"),
    ]
    .into_iter()
    .flatten()
    .collect::<Vec<String>>()
    .join(" · ")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kb() -> KeybindingsManager {
        KeybindingsManager::new()
    }

    fn bash_request(allow_prefix: Option<&str>, risk_reason: Option<&str>) -> ToolApprovalRequest {
        ToolApprovalRequest {
            request_id: "req-1".to_string(),
            kind: ToolApprovalKind::BashCommand,
            summary: "Run bash command".to_string(),
            detail: "cargo test --workspace".to_string(),
            risk_reason: risk_reason.map(str::to_string),
            allow_prefix: allow_prefix.map(str::to_string),
        }
    }

    #[test]
    fn bash_request_with_prefix_and_no_risk_offers_session_prefix_hotkey() {
        let mut panel = ApprovalPanel::new(bash_request(Some("cargo test"), None));
        assert_eq!(panel.request_id(), "req-1");
        assert_eq!(
            panel.handle_key(&kb(), "s"),
            ApprovalAction::Resolve(ToolApprovalDecision::AllowSessionPrefix {
                prefix: "cargo test".to_string(),
            })
        );
        assert_eq!(
            panel.handle_key(&kb(), "y"),
            ApprovalAction::Resolve(ToolApprovalDecision::AllowOnce)
        );
        assert_eq!(
            panel.handle_key(&kb(), "a"),
            ApprovalAction::Resolve(ToolApprovalDecision::SwitchToYolo)
        );
        assert_eq!(
            panel.handle_key(&kb(), "n"),
            ApprovalAction::Resolve(ToolApprovalDecision::Deny { reason: None })
        );
        assert_eq!(
            panel.handle_key(&kb(), "escape"),
            ApprovalAction::Resolve(ToolApprovalDecision::Deny { reason: None })
        );
    }

    #[test]
    fn bash_request_with_risk_omits_session_prefix_option() {
        let mut panel =
            ApprovalPanel::new(bash_request(Some("rm -rf"), Some("Destructive command")));
        assert_eq!(panel.handle_key(&kb(), "s"), ApprovalAction::None);
        // Down once from AllowOnce lands on SwitchToYolo (no session-prefix option in between).
        assert_eq!(panel.handle_key(&kb(), "down"), ApprovalAction::None);
        assert_eq!(
            panel.handle_key(&kb(), "enter"),
            ApprovalAction::Resolve(ToolApprovalDecision::SwitchToYolo)
        );
    }

    #[test]
    fn file_edit_request_offers_session_edits_and_truncates_preview() {
        let detail = (1..=10)
            .map(|i| format!("line {i}"))
            .collect::<Vec<_>>()
            .join("\n");
        let mut panel = ApprovalPanel::new(ToolApprovalRequest {
            request_id: "req-2".to_string(),
            kind: ToolApprovalKind::FileEdit,
            summary: "Edit src/main.rs".to_string(),
            detail,
            risk_reason: None,
            allow_prefix: None,
        });
        assert_eq!(
            panel.handle_key(&kb(), "s"),
            ApprovalAction::Resolve(ToolApprovalDecision::AllowSessionEdits)
        );
        let theme = crate::theme::Theme::builtin("prime", crate::theme::ColorMode::TrueColor);
        let rows: Vec<String> = panel
            .render(&theme, 80, &kb())
            .iter()
            .map(|line| line.iter().map(|s| s.content.as_str()).collect())
            .collect();
        assert!(rows.iter().any(|row| row.contains("Edit src/main.rs")));
        assert!(rows.iter().any(|row| row.contains("line 6")));
        assert!(!rows.iter().any(|row| row.contains("line 7")));
        assert!(rows.iter().any(|row| row.contains("(+4 more)")));
        assert!(rows
            .iter()
            .any(|row| row.contains("Allow edits for session")));
    }
}
