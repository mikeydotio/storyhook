use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::widgets::{Block, Borders, Clear};

use crate::tui::theme::Theme;

/// Render a modal overlay centered on the screen.
///
/// Clears the interior, draws a rounded border with the theme's modal_border
/// color, sets the given title, and returns the inner Rect for content.
pub fn render_modal(frame: &mut Frame, area: Rect, title: &str, theme: &Theme) -> Rect {
    let modal_area = centered_modal_rect(area);

    // Clear the background behind the modal
    frame.render_widget(Clear, modal_area);

    let block = Block::default()
        .title(format!(" {title} "))
        .borders(Borders::ALL)
        .border_type(ratatui::widgets::BorderType::Rounded)
        .border_style(theme.modal_border);

    let inner = block.inner(modal_area);
    frame.render_widget(block, modal_area);

    inner
}

/// Compute a centered rect that is approximately 66% wide and 75% tall.
pub fn centered_modal_rect(area: Rect) -> Rect {
    let modal_width = (area.width as u32 * 2 / 3) as u16;
    let modal_height = (area.height as u32 * 3 / 4) as u16;

    let x = area.x + (area.width.saturating_sub(modal_width)) / 2;
    let y = area.y + (area.height.saturating_sub(modal_height)) / 2;

    Rect::new(x, y, modal_width, modal_height)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn centered_rect_dimensions() {
        let area = Rect::new(0, 0, 120, 40);
        let modal = centered_modal_rect(area);

        // ~66% of 120 = 80
        assert_eq!(modal.width, 80);
        // ~75% of 40 = 30
        assert_eq!(modal.height, 30);
        // Centered horizontally: (120 - 80) / 2 = 20
        assert_eq!(modal.x, 20);
        // Centered vertically: (40 - 30) / 2 = 5
        assert_eq!(modal.y, 5);
    }

    #[test]
    fn centered_rect_small_terminal() {
        let area = Rect::new(0, 0, 30, 10);
        let modal = centered_modal_rect(area);

        // ~66% of 30 = 20
        assert_eq!(modal.width, 20);
        // ~75% of 10 = 7 (integer division: 10*3/4 = 7)
        assert_eq!(modal.height, 7);
        assert_eq!(modal.x, 5);
        // (10 - 7) / 2 = 1
        assert_eq!(modal.y, 1);
    }

    #[test]
    fn centered_rect_odd_dimensions() {
        let area = Rect::new(0, 0, 81, 25);
        let modal = centered_modal_rect(area);

        // 81 * 2 / 3 = 54
        assert_eq!(modal.width, 54);
        // 25 * 3 / 4 = 18 (integer: 75/4 = 18)
        assert_eq!(modal.height, 18);
        // (81 - 54) / 2 = 13
        assert_eq!(modal.x, 13);
        // (25 - 18) / 2 = 3
        assert_eq!(modal.y, 3);
    }
}
