use std::path::Path;
use std::time::Duration;

use crossterm::event::{self, Event, KeyCode, KeyModifiers};
use ratatui::style::{Color, Style};
use ratatui::text::Text;
use ratatui::widgets::Paragraph;
use ratatui::Frame;
use ratatui::layout::{Alignment, Constraint, Flex, Layout};

use crate::error::AppError;
use super::terminal;

pub fn run(_root: &Path) -> Result<(), AppError> {
    let mut term = terminal::init()?;

    let result = main_loop(&mut term);

    terminal::restore();
    result
}

fn main_loop(term: &mut ratatui::DefaultTerminal) -> Result<(), AppError> {
    loop {
        term.draw(render)?;

        if event::poll(Duration::from_millis(100))
            .map_err(|e| AppError::Storage(format!("event poll error: {e}")))?
            && let Event::Key(key) = event::read()
                .map_err(|e| AppError::Storage(format!("event read error: {e}")))?
        {
            if key.code == KeyCode::Char('q') {
                break;
            }
            if key.code == KeyCode::Char('c')
                && key.modifiers.contains(KeyModifiers::CONTROL)
            {
                break;
            }
        }
    }
    Ok(())
}

fn render(frame: &mut Frame) {
    let area = frame.area();
    let text = Text::styled("storyhook", Style::default().fg(Color::Cyan));
    let paragraph = Paragraph::new(text).alignment(Alignment::Center);

    // Center vertically by placing the paragraph in the middle of a vertical split.
    let [_, center, _] = Layout::vertical([
        Constraint::Fill(1),
        Constraint::Length(1),
        Constraint::Fill(1),
    ])
    .flex(Flex::Center)
    .areas(area);

    frame.render_widget(paragraph, center);
}
