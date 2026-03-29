pub mod board;
pub mod create_form;
pub mod dashboard;
pub mod filter_bar;
pub mod help;
pub mod modal;
pub mod status_bar;
pub mod story_detail;

use crossterm::event::{KeyEvent, MouseEvent};
use ratatui::layout::Rect;
use ratatui::Frame;

use super::action::Action;
use super::state::AppState;

/// A rectangular region of the screen that can receive mouse clicks.
#[derive(Debug, Clone)]
pub struct HitRegion {
    pub rect: Rect,
    pub target: HitTarget,
}

/// Identifies what a mouse click landed on.
#[derive(Debug, Clone)]
pub enum HitTarget {
    StoryRow { id: String },
    SectionHeader { state_slug: String },
    FilterChip { index: usize },
    DashboardSection { name: String },
    Button { id: String },
}

/// Trait for all renderable, interactive TUI components.
pub trait Component {
    /// Handle a keyboard event. Returns zero or more actions to dispatch.
    fn handle_key(&mut self, key: KeyEvent, state: &AppState) -> Vec<Action>;

    /// Handle a mouse event. Returns zero or more actions to dispatch.
    fn handle_mouse(&mut self, mouse: MouseEvent, state: &AppState) -> Vec<Action>;

    /// Render the component into the given frame area.
    fn render(&mut self, frame: &mut Frame, area: Rect, state: &AppState);

    /// Called after AppState changes so the component can update derived state.
    fn on_state_change(&mut self, _state: &AppState) {}

    /// Return clickable regions for mouse hit testing.
    fn hit_regions(&self) -> &[HitRegion] {
        &[]
    }
}
