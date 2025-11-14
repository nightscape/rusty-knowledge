use query_render::OperationWiring;
use r3bl_tui::TuiColor;

/// Intermediate representation of UI elements with operations attached.
/// This separates the interpretation phase (RenderSpec → UIElement)
/// from the rendering phase (UIElement → RenderOpIRVec).
#[derive(Debug, Clone)]
pub enum UIElement {
    Text {
        content: String,
        fg_color: Option<TuiColor>,
        bg_color: Option<TuiColor>,
    },
    EditableText {
        content: String,
        operations: Vec<OperationWiring>,
        fg_color: Option<TuiColor>,
        bg_color: Option<TuiColor>,
    },
    Checkbox {
        checked: bool,
        operations: Vec<OperationWiring>,
    },
    Badge {
        content: String,
        color: TuiColor,
    },
    Icon {
        symbol: String,
    },
    Row {
        children: Vec<UIElement>,
    },
}

impl UIElement {
    /// Get the first operation associated with this element, if any.
    /// For Row elements, searches children recursively to find a Checkbox or EditableText with operations.
    pub fn get_operation(&self) -> Option<&OperationWiring> {
        match self {
            UIElement::Checkbox { operations, .. } => operations.first(),
            UIElement::EditableText { operations, .. } => operations.first(),
            UIElement::Row { children } => {
                // Search children recursively for a checkbox or editable_text with operations
                for child in children {
                    if let Some(op) = child.get_operation() {
                        return Some(op);
                    }
                }
                None
            }
            _ => None,
        }
    }

    /// Check if this element is editable (EditableText)
    pub fn is_editable(&self) -> bool {
        matches!(self, UIElement::EditableText { .. })
    }

    /// Find the first EditableText child recursively, if any
    pub fn find_editable_text(&self) -> Option<&UIElement> {
        match self {
            UIElement::EditableText { .. } => Some(self),
            UIElement::Row { children } => {
                for child in children {
                    if let Some(editable) = child.find_editable_text() {
                        return Some(editable);
                    }
                }
                None
            }
            _ => None,
        }
    }
}
