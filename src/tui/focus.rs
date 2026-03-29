/// The base view that receives keyboard input when no modal is open.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FocusTarget {
    Dashboard,
    Board,
}

/// A modal overlay that captures all keyboard input when active.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Modal {
    StoryDetail { story_id: String },
    CreateForm,
    Help,
}

/// A stack of modals on top of a base focus target.
///
/// When `modals` is non-empty, the top modal captures all keyboard input.
/// Mouse clicks outside a modal close it.
#[derive(Debug, Clone)]
pub struct FocusStack {
    pub base: FocusTarget,
    pub modals: Vec<Modal>,
}

impl FocusStack {
    pub fn new(base: FocusTarget) -> Self {
        Self {
            base,
            modals: Vec::new(),
        }
    }

    /// Push a modal onto the stack. It becomes the new input target.
    pub fn push_modal(&mut self, modal: Modal) {
        self.modals.push(modal);
    }

    /// Pop the top modal off the stack. Returns `None` if the stack is empty.
    pub fn pop_modal(&mut self) -> Option<Modal> {
        self.modals.pop()
    }

    /// Peek at the top modal without removing it.
    pub fn top_modal(&self) -> Option<&Modal> {
        self.modals.last()
    }

    /// Returns `true` if any modal is currently open.
    pub fn has_modal(&self) -> bool {
        !self.modals.is_empty()
    }

    /// Returns a reference to the base focus target.
    pub fn base(&self) -> &FocusTarget {
        &self.base
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_stack_has_no_modals() {
        let stack = FocusStack::new(FocusTarget::Board);
        assert!(!stack.has_modal());
        assert_eq!(stack.top_modal(), None);
    }

    #[test]
    fn push_modal_makes_it_top() {
        let mut stack = FocusStack::new(FocusTarget::Dashboard);
        stack.push_modal(Modal::Help);
        assert!(stack.has_modal());
        assert_eq!(stack.top_modal(), Some(&Modal::Help));
    }

    #[test]
    fn pop_modal_returns_top() {
        let mut stack = FocusStack::new(FocusTarget::Board);
        stack.push_modal(Modal::CreateForm);
        let popped = stack.pop_modal();
        assert_eq!(popped, Some(Modal::CreateForm));
        assert!(!stack.has_modal());
    }

    #[test]
    fn nested_modals_maintain_order() {
        let mut stack = FocusStack::new(FocusTarget::Board);
        stack.push_modal(Modal::StoryDetail {
            story_id: "SH-1".to_string(),
        });
        stack.push_modal(Modal::Help);

        assert_eq!(stack.top_modal(), Some(&Modal::Help));
        stack.pop_modal();
        assert_eq!(
            stack.top_modal(),
            Some(&Modal::StoryDetail {
                story_id: "SH-1".to_string()
            })
        );
        stack.pop_modal();
        assert!(!stack.has_modal());
    }

    #[test]
    fn pop_empty_is_safe() {
        let mut stack = FocusStack::new(FocusTarget::Dashboard);
        assert_eq!(stack.pop_modal(), None);
        assert_eq!(stack.pop_modal(), None);
        assert!(!stack.has_modal());
    }

    #[test]
    fn base_is_preserved_through_modal_operations() {
        let mut stack = FocusStack::new(FocusTarget::Board);
        assert_eq!(stack.base(), &FocusTarget::Board);
        stack.push_modal(Modal::Help);
        assert_eq!(stack.base(), &FocusTarget::Board);
        stack.pop_modal();
        assert_eq!(stack.base(), &FocusTarget::Board);
    }

    #[test]
    fn triple_nested_modals() {
        let mut stack = FocusStack::new(FocusTarget::Board);
        stack.push_modal(Modal::StoryDetail {
            story_id: "SH-5".to_string(),
        });
        stack.push_modal(Modal::CreateForm);
        stack.push_modal(Modal::Help);

        assert_eq!(stack.modals.len(), 3);
        assert_eq!(stack.top_modal(), Some(&Modal::Help));

        stack.pop_modal();
        assert_eq!(stack.top_modal(), Some(&Modal::CreateForm));
        stack.pop_modal();
        assert_eq!(
            stack.top_modal(),
            Some(&Modal::StoryDetail {
                story_id: "SH-5".to_string()
            })
        );
        stack.pop_modal();
        assert!(!stack.has_modal());
    }
}
