//! Sort-order picker dialog: the core `SortOrder`s plus any sort modes
//! contributed by active plugins (`session-list-sort-key` slot). Picking a
//! plugin mode never replaces the core order silently; it composes on top
//! (sessions re-rank inside their groups) and shows here as the current
//! choice until cleared.

use crossterm::event::{KeyCode, KeyEvent};
use ratatui::prelude::*;
use ratatui::widgets::*;

use super::DialogResult;
use crate::session::config::SortOrder;
use crate::tui::styles::Theme;

const CORE_OPTIONS: &[SortOrder] = &[
    SortOrder::Newest,
    SortOrder::Attention,
    SortOrder::LastActivity,
    SortOrder::Oldest,
    SortOrder::AZ,
    SortOrder::ZA,
];

/// What the picker returns: a core order (clears any plugin mode) or a
/// plugin-contributed sort mode.
#[derive(Debug, Clone, PartialEq)]
pub enum SortChoice {
    Core(SortOrder),
    Plugin {
        plugin_id: String,
        contribution_id: String,
    },
}

struct Row {
    choice: SortChoice,
    label: String,
}

pub struct SortPickerDialog {
    rows: Vec<Row>,
    selected: usize,
    current: SortChoice,
    list_area: Rect,
    dialog_area: Rect,
}

impl SortPickerDialog {
    pub fn new(current_order: SortOrder, current_plugin: Option<(String, String)>) -> Self {
        let mut rows: Vec<Row> = CORE_OPTIONS
            .iter()
            .map(|o| Row {
                choice: SortChoice::Core(*o),
                label: o.label().to_string(),
            })
            .collect();
        for (plugin_id, contribution) in
            crate::plugin::ui::declared(aoe_plugin_api::UiSlot::SessionListSortKey)
        {
            rows.push(Row {
                label: format!("{} ({plugin_id})", contribution.title),
                choice: SortChoice::Plugin {
                    plugin_id,
                    contribution_id: contribution.id,
                },
            });
        }
        // A persisted plugin sort whose contribution vanished (plugin
        // disabled/uninstalled) falls back to the core order, not to row 0:
        // otherwise Enter on the pre-selected row silently switches the
        // user to Newest.
        let current = current_plugin
            .map(|(plugin_id, contribution_id)| SortChoice::Plugin {
                plugin_id,
                contribution_id,
            })
            .filter(|choice| rows.iter().any(|r| &r.choice == choice))
            .unwrap_or(SortChoice::Core(current_order));
        let selected = rows.iter().position(|r| r.choice == current).unwrap_or(0);
        Self {
            rows,
            selected,
            current,
            list_area: Rect::default(),
            dialog_area: Rect::default(),
        }
    }

    fn row_to_idx(&self, col: u16, row: u16) -> Option<usize> {
        if !self
            .list_area
            .contains(ratatui::layout::Position::from((col, row)))
        {
            return None;
        }
        let i = (row - self.list_area.y) as usize;
        if i >= self.rows.len() {
            return None;
        }
        Some(i)
    }

    pub fn handle_click(&mut self, col: u16, row: u16) -> DialogResult<SortChoice> {
        if !self
            .dialog_area
            .contains(ratatui::layout::Position::from((col, row)))
        {
            return DialogResult::Cancel;
        }
        let Some(idx) = self.row_to_idx(col, row) else {
            return DialogResult::Continue;
        };
        self.selected = idx;
        DialogResult::Submit(self.rows[idx].choice.clone())
    }

    pub fn handle_hover(&mut self, col: u16, row: u16) -> bool {
        let Some(idx) = self.row_to_idx(col, row) else {
            return false;
        };
        if self.selected == idx {
            return false;
        }
        self.selected = idx;
        true
    }

    pub fn handle_key(&mut self, key: KeyEvent) -> DialogResult<SortChoice> {
        match key.code {
            KeyCode::Esc => DialogResult::Cancel,
            KeyCode::Up | KeyCode::Char('k') => {
                if self.selected > 0 {
                    self.selected -= 1;
                }
                DialogResult::Continue
            }
            KeyCode::Down | KeyCode::Char('j') => {
                if self.selected + 1 < self.rows.len() {
                    self.selected += 1;
                }
                DialogResult::Continue
            }
            KeyCode::Enter => DialogResult::Submit(self.rows[self.selected].choice.clone()),
            _ => DialogResult::Continue,
        }
    }

    pub fn render(&mut self, frame: &mut Frame, area: Rect, theme: &Theme) {
        let widest = self
            .rows
            .iter()
            .map(|r| r.label.chars().count())
            .max()
            .unwrap_or(0) as u16;
        let dialog_width: u16 = (widest + 16).clamp(32, 60);
        let dialog_height: u16 = self.rows.len() as u16 + 5;

        let dialog_area = super::centered_rect(area, dialog_width, dialog_height);
        self.dialog_area = dialog_area;
        frame.render_widget(Clear, dialog_area);

        let block = Block::default()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(Style::default().fg(theme.accent))
            .title(" Sort Order ")
            .title_style(Style::default().fg(theme.title).bold());

        let inner = block.inner(dialog_area);
        frame.render_widget(block, dialog_area);

        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .margin(1)
            .constraints([Constraint::Min(1), Constraint::Length(1)])
            .split(inner);

        let mut lines: Vec<Line> = Vec::new();
        for (i, row) in self.rows.iter().enumerate() {
            let is_selected = i == self.selected;
            let prefix = if is_selected { "> " } else { "  " };
            let name_style = if is_selected {
                Style::default().fg(theme.accent).bold()
            } else {
                Style::default().fg(theme.text)
            };
            let mut spans = vec![
                Span::styled(prefix, name_style),
                Span::styled(row.label.clone(), name_style),
            ];
            if row.choice == self.current {
                spans.push(Span::styled(
                    "  (current)",
                    Style::default().fg(theme.running),
                ));
            }
            lines.push(Line::from(spans));
        }
        self.list_area = chunks[0];
        frame.render_widget(Paragraph::new(lines), chunks[0]);

        let hint = Line::from(vec![
            Span::styled("Enter", Style::default().fg(theme.hint)),
            Span::raw(" select  "),
            Span::styled("Esc", Style::default().fg(theme.hint)),
            Span::raw(" close"),
        ]);
        frame.render_widget(Paragraph::new(hint), chunks[1]);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::KeyModifiers;

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    #[test]
    fn test_new_selects_current() {
        let dialog = SortPickerDialog::new(SortOrder::LastActivity, None);
        assert_eq!(dialog.selected, 2);
    }

    #[test]
    fn test_esc_cancels() {
        let mut dialog = SortPickerDialog::new(SortOrder::Newest, None);
        assert!(matches!(
            dialog.handle_key(key(KeyCode::Esc)),
            DialogResult::Cancel
        ));
    }

    #[test]
    fn test_enter_submits_selection() {
        let mut dialog = SortPickerDialog::new(SortOrder::Newest, None);
        dialog.handle_key(key(KeyCode::Down));
        dialog.handle_key(key(KeyCode::Down));
        let result = dialog.handle_key(key(KeyCode::Enter));
        assert!(matches!(
            result,
            DialogResult::Submit(SortChoice::Core(SortOrder::LastActivity))
        ));
    }

    #[test]
    fn test_navigation_clamps() {
        let mut dialog = SortPickerDialog::new(SortOrder::Newest, None);
        dialog.handle_key(key(KeyCode::Up));
        assert_eq!(dialog.selected, 0);
        for _ in 0..30 {
            dialog.handle_key(key(KeyCode::Down));
        }
        assert!(dialog.selected >= CORE_OPTIONS.len() - 1);
    }
}
