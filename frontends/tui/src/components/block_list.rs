use crate::render_interpreter::RenderInterpreter;
use crate::state::{AppSignal, State};
use crate::ui_element::UIElement;
use crate::config::{BindingContext, Action};
use r3bl_tui::{
    BoxedSafeComponent, CommonResult, Component, EventPropagation, FlexBox, FlexBoxId,
    GlobalData, HasFocus, InputEvent, Key, KeyPress, KeyState, RenderPipeline, SpecialKey,
    SurfaceBounds, TerminalWindowMainThreadSignal, render_pipeline, send_signal, throws_with_return, ZOrder,
    EditorEngineApplyEventResult, SystemClipboard, engine_public_api, row, EditorEngine, EditorEngineConfig,
    Size, width, height, EditorEvent,
};

// Helper function to extract field name from OperationWiring
// For "set_field" operations, tries to extract from descriptor params, otherwise uses modified_param
fn get_field_name(op: &query_render::OperationWiring) -> String {
    // For "set_field" operations, the field name is typically in modified_param
    // or we can try to extract it from the operation descriptor
    // For now, use modified_param as it often matches the database field name
    op.modified_param.clone()
}

/// Component that displays and manages the block list with hierarchical structure
pub struct BlockListComponent {
    id: FlexBoxId,
    element_tree: Vec<UIElement>,
    /// Editor engine for handling text input (created once, reused)
    editor_engine: Option<EditorEngine>,
}

impl BlockListComponent {
    pub fn new(id: FlexBoxId) -> Self {
        Self {
            id,
            element_tree: Vec::new(),
            editor_engine: None,
        }
    }

    pub fn new_boxed(id: FlexBoxId) -> BoxedSafeComponent<State, AppSignal> {
        Box::new(Self::new(id))
    }

    /// Rebuild element tree from current state
    pub fn rebuild_element_tree(&mut self, global_data: &GlobalData<State, AppSignal>) {
        self.element_tree = RenderInterpreter::build_element_tree(
            &global_data.state.render_spec,
            &global_data.state.data,
            global_data.state.selected_index,
        );
    }

    /// Get the current context (Editing or Navigation)
    fn get_current_context(&self, global_data: &GlobalData<State, AppSignal>) -> BindingContext {
        if global_data.state.editing_block_index.is_some() {
            BindingContext::Editing
        } else {
            BindingContext::Navigation
        }
    }

    /// Handle a key binding action
    fn handle_key_binding_action(
        &mut self,
        action: &Action,
        global_data: &mut GlobalData<State, AppSignal>,
    ) -> CommonResult<EventPropagation> {
        throws_with_return!({
            match action {
                Action::Operation(op_name) => {
                    // Execute operation on selected block
                    match global_data.state.execute_operation_on_selected(op_name, None) {
                        Ok(_) => {
                            global_data.state.status_message = format!("Executing {}...", op_name);
                            EventPropagation::ConsumedRender
                        }
                        Err(e) => {
                            global_data.state.status_message = format!("{} failed: {}", op_name, e);
                            EventPropagation::ConsumedRender
                        }
                    }
                }
                Action::Special(special_action) => {
                    match special_action.as_str() {
                        "toggle_completion" => {
                            // Handle toggle completion (the 'x' key behavior)
                            if global_data.state.selected_index < self.element_tree.len() {
                                let element = &self.element_tree[global_data.state.selected_index];
                                if let Some(operation) = element.get_operation() {
                                    if let Some(row) = global_data.state.data.get(global_data.state.selected_index) {
                                        if let Some(id_value) = row.get(&operation.descriptor.id_column) {
                                            if let Some(id_str) = id_value.as_string() {
                                                if let Some(field_value) = row.get(&get_field_name(operation)) {
                                                    let bool_val = match field_value {
                                                        rusty_knowledge::storage::types::Value::Boolean(b) => *b,
                                                        rusty_knowledge::storage::types::Value::Integer(i) => *i != 0,
                                                        _ => {
                                                            global_data.state.status_message = format!("Field {} is not a boolean or integer", get_field_name(operation));
                                                            return Ok(EventPropagation::ConsumedRender);
                                                        }
                                                    };
                                                    let op_signal = AppSignal::ExecuteOperation {
                                                        operation_name: operation.descriptor.name.clone(),
                                                        table: operation.descriptor.table.clone(),
                                                        id_column: operation.descriptor.id_column.clone(),
                                                        id_value: id_str.to_string(),
                                                        field: get_field_name(operation),
                                                        new_value: rusty_knowledge::storage::types::Value::Boolean(!bool_val),
                                                    };
                                                    send_signal!(
                                                        global_data.main_thread_channel_sender,
                                                        TerminalWindowMainThreadSignal::ApplyAppSignal(op_signal)
                                                    );
                                                    global_data.state.status_message = "Toggling completion...".to_string();
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                            EventPropagation::ConsumedRender
                        }
                        "start_editing" => {
                            // Start editing the selected block
                            return self.start_editing_selected_block(global_data);
                        }
                        "save_and_exit" => {
                            // Save current editing block and exit edit mode
                            if self.save_current_editing_block(global_data) {
                                EventPropagation::ConsumedRender
                            } else {
                                EventPropagation::Propagate
                            }
                        }
                        "split_block" => {
                            // Split block at cursor position
                            let cursor_offset = match global_data.state.calculate_cursor_offset() {
                                Ok(offset) => offset,
                                Err(e) => {
                                    global_data.state.status_message = format!("Split failed: {}", e);
                                    return Ok(EventPropagation::ConsumedRender);
                                }
                            };
                            let block_id = match global_data.state.selected_block_id() {
                                Some(id) => id,
                                None => {
                                    global_data.state.status_message = "Split failed: No block selected".to_string();
                                    return Ok(EventPropagation::ConsumedRender);
                                }
                            };
                            let ui_state = rusty_knowledge::api::render_engine::UiState {
                                cursor_pos: Some(rusty_knowledge::api::render_engine::CursorPosition {
                                    block_id: block_id.clone(),
                                    offset: cursor_offset,
                                }),
                                focused_id: Some(block_id),
                            };
                            match global_data.state.execute_operation_on_selected("split_block", Some(ui_state)) {
                                Ok(_) => {
                                    global_data.state.status_message = "Splitting block...".to_string();
                                    // Exit edit mode after split
                                    global_data.state.editing_block_index = None;
                                    global_data.state.editing_buffer = None;
                                    self.editor_engine = None;
                                    EventPropagation::ConsumedRender
                                }
                                Err(e) => {
                                    global_data.state.status_message = format!("Split failed: {}", e);
                                    EventPropagation::ConsumedRender
                                }
                            }
                        }
                        _ => {
                            global_data.state.status_message = format!("Unknown special action: {}", special_action);
                            EventPropagation::ConsumedRender
                        }
                    }
                }
            }
        });
    }

    /// Save the currently editing block if there is one
    /// Returns true if a block was saved, false otherwise
    pub fn save_current_editing_block(
        &mut self,
        global_data: &mut GlobalData<State, AppSignal>,
    ) -> bool {
        // Check if we're editing a block
        if let Some(editing_idx) = global_data.state.editing_block_index {
            if let Some(buffer) = &global_data.state.editing_buffer {
                // Extract operation info from the EditableText
                let operation_info = if let Some(element) = self.element_tree.get(editing_idx) {
                    element.find_editable_text()
                        .and_then(|editable| {
                            if let UIElement::EditableText { operations, .. } = editable {
                                operations.first().map(|op| (
                                    op.descriptor.name.clone(),
                                    op.descriptor.table.clone(),
                                    op.descriptor.id_column.clone(),
                                    get_field_name(op),
                                ))
                            } else {
                                None
                            }
                        })
                } else {
                    None
                };

                // Get content from buffer - join all lines with \n
                let lines = buffer.get_lines();
                let mut buffer_content = String::new();
                let mut i = 0;
                loop {
                    if let Some(line) = lines.get_line_content(row(i)) {
                        if !buffer_content.is_empty() {
                            buffer_content.push('\n');
                        }
                        buffer_content.push_str(line);
                        i += 1;
                    } else {
                        break;
                    }
                }

                // Exit edit mode and clear buffer first (release mutable borrow)
                global_data.state.editing_block_index = None;
                global_data.state.editing_buffer = None;
                // Clear editor engine when exiting edit mode
                self.editor_engine = None;

                // Now we can borrow state.data immutably
                if let Some((operation_name, table, id_column, field)) = operation_info {
                    if let Some(row_data) = global_data.state.data.get(editing_idx) {
                        if let Some(id_value) = row_data.get(&id_column) {
                            if let Some(id_str) = id_value.as_string() {
                                let new_value = rusty_knowledge::storage::types::Value::String(buffer_content);
                                let op_signal = AppSignal::ExecuteOperation {
                                    operation_name: operation_name.clone(),
                                    table,
                                    id_column,
                                    id_value: id_str.to_string(),
                                    field,
                                    new_value,
                                };
                                send_signal!(
                                    global_data.main_thread_channel_sender,
                                    TerminalWindowMainThreadSignal::ApplyAppSignal(op_signal)
                                );
                                global_data.state.status_message = "Saving...".to_string();
                                return true;
                            }
                        }
                    }
                }
            }
        }
        false
    }

    /// Start editing the currently selected block
    fn start_editing_selected_block(
        &mut self,
        global_data: &mut GlobalData<State, AppSignal>,
    ) -> CommonResult<EventPropagation> {
        throws_with_return!({
            if global_data.state.selected_index < self.element_tree.len() {
                let element = &self.element_tree[global_data.state.selected_index];
                // Search recursively for EditableText (might be inside a Row)
                if let Some(editable_text) = element.find_editable_text() {
                    if let UIElement::EditableText { content, .. } = editable_text {
                        // Create editor buffer with initial content
                        // Split content by newlines to initialize buffer with multiple lines
                        let mut buffer = r3bl_tui::EditorBuffer::new_empty(None, None);
                        let lines: Vec<&str> = content.split('\n').collect();
                        buffer.init_with(lines);

                        // Create editor engine and move cursor to end
                        let mut editor_engine = EditorEngine::new(EditorEngineConfig {
                            multiline_mode: r3bl_tui::LineMode::MultiLine,
                            syntax_highlight: r3bl_tui::SyntaxHighlightMode::Disable,
                            edit_mode: r3bl_tui::EditMode::ReadWrite,
                        });

                        // CRITICAL: Set viewport on EditorEngine before processing events
                        // Operations require a valid viewport size
                        editor_engine.current_box.style_adjusted_bounds_size = width(200) + height(50);

                        // Move cursor to end of initial content
                        let end_key_event = InputEvent::Keyboard(KeyPress::Plain {
                            key: Key::SpecialKey(SpecialKey::End),
                        });
                        let _ = engine_public_api::apply_event(
                            &mut buffer,
                            &mut editor_engine,
                            end_key_event,
                            &mut SystemClipboard,
                        );

                        global_data.state.editing_buffer = Some(buffer);
                        self.editor_engine = Some(editor_engine);

                        global_data.state.editing_block_index = Some(global_data.state.selected_index);
                        global_data.state.status_message = "Editing... (Esc or Ctrl+Enter to save, Enter for newline)".to_string();
                        EventPropagation::ConsumedRender
                    } else {
                        EventPropagation::Propagate
                    }
                } else {
                    EventPropagation::Propagate
                }
            } else {
                EventPropagation::Propagate
            }
        });
    }
}

impl Component<State, AppSignal> for BlockListComponent {
    fn get_id(&self) -> FlexBoxId {
        self.id
    }

    fn reset(&mut self) {
        // Clear editor engine when resetting
        self.editor_engine = None;
    }

    fn handle_event(
        &mut self,
        global_data: &mut GlobalData<State, AppSignal>,
        input_event: InputEvent,
        has_focus: &mut HasFocus,
    ) -> CommonResult<EventPropagation> {
        throws_with_return!({
            // Only handle events if this component has focus
            if !has_focus.does_id_have_focus(self.id) {
                return Ok(EventPropagation::Propagate);
            }

            // Rebuild element tree to ensure it's up to date
            self.rebuild_element_tree(global_data);

            // Ensure we're always editing the selected block (unless we're already editing it)
            if global_data.state.editing_block_index != Some(global_data.state.selected_index) {
                // We should be editing but aren't, or we're editing a different block
                // Start editing the selected block
                if global_data.state.selected_index < self.element_tree.len() {
                    let element = &self.element_tree[global_data.state.selected_index];
                    if element.find_editable_text().is_some() {
                        // Save current editing block if we were editing a different one
                        if let Some(editing_idx) = global_data.state.editing_block_index {
                            if editing_idx != global_data.state.selected_index {
                                // Save the previous block before switching
                                if let Some(buffer) = &global_data.state.editing_buffer {
                                    let lines = buffer.get_lines();
                                    if let Some(element) = self.element_tree.get(editing_idx) {
                                        if let Some(editable_text) = element.find_editable_text() {
                                            if let UIElement::EditableText { operations, .. } = editable_text {
                                                if let Some(op) = operations.first() {
                                                    let mut buffer_content = String::new();
                                                    let mut i = 0;
                                                    loop {
                                                        if let Some(line) = lines.get_line_content(row(i)) {
                                                            if !buffer_content.is_empty() {
                                                                buffer_content.push('\n');
                                                            }
                                                            buffer_content.push_str(line);
                                                            i += 1;
                                                        } else {
                                                            break;
                                                        }
                                                    }

                                                    if let Some(row_data) = global_data.state.data.get(editing_idx) {
                                                        if let Some(id_value) = row_data.get(&op.descriptor.id_column) {
                                                            if let Some(id_str) = id_value.as_string() {
                                                                let new_value = rusty_knowledge::storage::types::Value::String(buffer_content);
                                                                let op_signal = AppSignal::ExecuteOperation {
                                                                    operation_name: op.descriptor.name.clone(),
                                                                    table: op.descriptor.table.clone(),
                                                                    id_column: op.descriptor.id_column.clone(),
                                                                    id_value: id_str.to_string(),
                                                                    field: get_field_name(op),
                                                                    new_value,
                                                                };
                                                                send_signal!(
                                                                    global_data.main_thread_channel_sender,
                                                                    TerminalWindowMainThreadSignal::ApplyAppSignal(op_signal)
                                                                );
                                                            }
                                                        }
                                                    }
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                        }
                        // Now start editing the selected block
                        return self.start_editing_selected_block(global_data);
                    }
                }
            }

            let mut event_consumed = false;

            match input_event {
                InputEvent::Keyboard(KeyPress::Plain { key }) => {
                    // DEBUG: Log all Plain events when editing
                    if global_data.state.editing_block_index.is_some() {
                        global_data.state.status_message = format!("Plain event: key={:?}", key);
                    }

                    // Handle edit mode first - use EditorEngine to process input
                    if let Some(_editing_idx) = global_data.state.editing_block_index {
                        if let Some(buffer) = &mut global_data.state.editing_buffer {
                            // Get or create editor engine (reuse across events)
                            // Note: Editor engine should already exist from Enter handler, but create if missing
                            if self.editor_engine.is_none() {
                                self.editor_engine = Some(EditorEngine::new(EditorEngineConfig {
                                    multiline_mode: r3bl_tui::LineMode::MultiLine,
                                    syntax_highlight: r3bl_tui::SyntaxHighlightMode::Disable,
                                    edit_mode: r3bl_tui::EditMode::ReadWrite,
                                }));
                            }

                            let editor_engine = self.editor_engine.as_mut().unwrap();

                            // CRITICAL: Set viewport on EditorEngine before processing events
                            // Operations like buffer.get_mut(engine.viewport()) require a valid viewport
                            // Use a reasonable default if not set (80 cols is standard terminal width)
                            if editor_engine.viewport() == Size::default() {
                                editor_engine.current_box.style_adjusted_bounds_size = width(200) + height(50);
                            }

                            // Handle Esc: save and exit edit mode (check BEFORE passing to EditorEngine)
                            if let Key::SpecialKey(SpecialKey::Esc) = key {
                                // Escape: save and exit edit mode
                                global_data.state.status_message = "Esc - saving and exiting".to_string();

                                // Extract operation info and save
                                let operation_info = if let Some(element) = self.element_tree.get(global_data.state.editing_block_index.unwrap()) {
                                    element.find_editable_text()
                                        .and_then(|editable| {
                                            if let UIElement::EditableText { operations, .. } = editable {
                                                operations.first().map(|op| (
                                                    op.descriptor.name.clone(),
                                                    op.descriptor.table.clone(),
                                                    op.descriptor.id_column.clone(),
                                                    get_field_name(op),
                                                ))
                                            } else {
                                                None
                                            }
                                        })
                                } else {
                                    None
                                };

                                // Get content from buffer - join all lines with \n
                                let lines = buffer.get_lines();
                                let mut buffer_content = String::new();
                                let mut i = 0;
                                loop {
                                    if let Some(line) = lines.get_line_content(row(i)) {
                                        if !buffer_content.is_empty() {
                                            buffer_content.push('\n');
                                        }
                                        buffer_content.push_str(line);
                                        i += 1;
                                    } else {
                                        break;
                                    }
                                }

                                // Exit edit mode and clear buffer first (release mutable borrow)
                                let editing_idx = global_data.state.editing_block_index.unwrap();
                                global_data.state.editing_block_index = None;
                                global_data.state.editing_buffer = None;
                                // Clear editor engine when exiting edit mode
                                self.editor_engine = None;

                                // Now we can borrow state.data immutably
                                if let Some((operation_name, table, id_column, field)) = operation_info {
                                    if let Some(row_data) = global_data.state.data.get(editing_idx) {
                                        if let Some(id_value) = row_data.get(&id_column) {
                                            if let Some(id_str) = id_value.as_string() {
                                                let new_value = rusty_knowledge::storage::types::Value::String(buffer_content);
                                                let op_signal = AppSignal::ExecuteOperation {
                                                    operation_name: operation_name.clone(),
                                                    table,
                                                    id_column,
                                                    id_value: id_str.to_string(),
                                                    field,
                                                    new_value,
                                                };
                                                send_signal!(
                                                    global_data.main_thread_channel_sender,
                                                    TerminalWindowMainThreadSignal::ApplyAppSignal(op_signal)
                                                );
                                                global_data.state.status_message = "Saving...".to_string();
                                            }
                                        }
                                    }
                                }
                                return Ok(EventPropagation::ConsumedRender);
                            }

                            // Handle Up/Down arrows: check if at boundary, otherwise let EditorEngine handle
                            if let Key::SpecialKey(SpecialKey::Up) = key {
                                let lines = buffer.get_lines();
                                let caret_raw = buffer.get_caret_raw();
                                let caret_row = caret_raw.row_index.as_usize();

                                // Check if we're at the first line
                                if caret_row == 0 {
                                    // At first line: save current block and move to previous
                                    let editing_idx = global_data.state.editing_block_index.unwrap();
                                    if let Some(element) = self.element_tree.get(editing_idx) {
                                        if let Some(editable_text) = element.find_editable_text() {
                                            if let UIElement::EditableText { operations, .. } = editable_text {
                                                if let Some(op) = operations.first() {
                                                    let mut buffer_content = String::new();
                                                    let mut i = 0;
                                                    loop {
                                                        if let Some(line) = lines.get_line_content(row(i)) {
                                                            if !buffer_content.is_empty() {
                                                                buffer_content.push('\n');
                                                            }
                                                            buffer_content.push_str(line);
                                                            i += 1;
                                                        } else {
                                                            break;
                                                        }
                                                    }

                                                    if let Some(row_data) = global_data.state.data.get(editing_idx) {
                                                        if let Some(id_value) = row_data.get(&op.descriptor.id_column) {
                                                            if let Some(id_str) = id_value.as_string() {
                                                                let new_value = rusty_knowledge::storage::types::Value::String(buffer_content);
                                                                let op_signal = AppSignal::ExecuteOperation {
                                                                    operation_name: op.descriptor.name.clone(),
                                                                    table: op.descriptor.table.clone(),
                                                                    id_column: op.descriptor.id_column.clone(),
                                                                    id_value: id_str.to_string(),
                                                                    field: get_field_name(op),
                                                                    new_value,
                                                                };
                                                                send_signal!(
                                                                    global_data.main_thread_channel_sender,
                                                                    TerminalWindowMainThreadSignal::ApplyAppSignal(op_signal)
                                                                );
                                                            }
                                                        }
                                                    }
                                                }
                                            }
                                        }
                                    }

                                    // Exit edit mode and move to previous
                                    global_data.state.editing_block_index = None;
                                    global_data.state.editing_buffer = None;
                                    self.editor_engine = None;

                                    global_data.state.select_previous();
                                    // Start editing the new block
                                    return self.start_editing_selected_block(global_data);
                                }
                                // Not at first line: pass to EditorEngine to move cursor up within text
                                // Fall through to EditorEngine handler below
                            }

                            if let Key::SpecialKey(SpecialKey::Down) = key {
                                let lines = buffer.get_lines();
                                let caret_raw = buffer.get_caret_raw();
                                let caret_row = caret_raw.row_index.as_usize();

                                // Count total lines in buffer
                                let mut line_count: usize = 0;
                                loop {
                                    if lines.get_line_content(row(line_count)).is_some() {
                                        line_count += 1;
                                    } else {
                                        break;
                                    }
                                }

                                // Check if we're at the last line (caret_row is 0-indexed, so last line is line_count - 1)
                                let is_last_line = caret_row >= line_count.saturating_sub(1);

                                if is_last_line {
                                    // At last line: save current block and move to next
                                    let editing_idx = global_data.state.editing_block_index.unwrap();
                                    if let Some(element) = self.element_tree.get(editing_idx) {
                                        if let Some(editable_text) = element.find_editable_text() {
                                            if let UIElement::EditableText { operations, .. } = editable_text {
                                                if let Some(op) = operations.first() {
                                                    let mut buffer_content = String::new();
                                                    let mut i = 0;
                                                    loop {
                                                        if let Some(line) = lines.get_line_content(row(i)) {
                                                            if !buffer_content.is_empty() {
                                                                buffer_content.push('\n');
                                                            }
                                                            buffer_content.push_str(line);
                                                            i += 1;
                                                        } else {
                                                            break;
                                                        }
                                                    }

                                                    if let Some(row_data) = global_data.state.data.get(editing_idx) {
                                                        if let Some(id_value) = row_data.get(&op.descriptor.id_column) {
                                                            if let Some(id_str) = id_value.as_string() {
                                                                let new_value = rusty_knowledge::storage::types::Value::String(buffer_content);
                                                                let op_signal = AppSignal::ExecuteOperation {
                                                                    operation_name: op.descriptor.name.clone(),
                                                                    table: op.descriptor.table.clone(),
                                                                    id_column: op.descriptor.id_column.clone(),
                                                                    id_value: id_str.to_string(),
                                                                    field: get_field_name(op),
                                                                    new_value,
                                                                };
                                                                send_signal!(
                                                                    global_data.main_thread_channel_sender,
                                                                    TerminalWindowMainThreadSignal::ApplyAppSignal(op_signal)
                                                                );
                                                            }
                                                        }
                                                    }
                                                }
                                            }
                                        }
                                    }

                                    // Exit edit mode and move to next
                                    global_data.state.editing_block_index = None;
                                    global_data.state.editing_buffer = None;
                                    self.editor_engine = None;

                                    global_data.state.select_next();
                                    // Start editing the new block
                                    return self.start_editing_selected_block(global_data);
                                }
                                // Not at last line: pass to EditorEngine to move cursor down within text
                                // Fall through to EditorEngine handler below
                            }

                            // Handle Enter: let EditorEngine insert newline (default behavior)
                            // Also check for Ctrl+Enter here in case it comes through as Plain
                            // Since many terminals don't send modifier keys reliably
                            if let Key::SpecialKey(SpecialKey::Enter) = key {
                                // Plain Enter: let EditorEngine handle it (inserts newline)
                                // Don't intercept - pass through to EditorEngine
                                global_data.state.status_message = "Enter - inserting newline (use Esc or Ctrl+Enter to save)".to_string();
                            }

                            // Check for Ctrl+Enter in Plain handler (some terminals send it as Plain)
                            // We can't detect Ctrl in Plain events, so we'll need a different approach
                            // For now, let's use a different key combination or check if there's a way to detect it
                            match engine_public_api::apply_event(
                                buffer,
                                editor_engine,
                                input_event.clone(),
                                &mut SystemClipboard,
                            ) {
                                Ok(EditorEngineApplyEventResult::Applied) => {
                                    // Event was handled by editor - consume and render
                                    return Ok(EventPropagation::ConsumedRender);
                                }
                                Ok(EditorEngineApplyEventResult::NotApplied) => {
                                    // Event not handled by editor - Esc should have been handled above
                                    // For other NotApplied events, still consume them (don't let them fall through)
                                    // The editor might not handle all events, but we're in edit mode so consume everything
                                    return Ok(EventPropagation::ConsumedRender);
                                }
                                Err(e) => {
                                    global_data.state.status_message = format!("Editor error: {}", e);
                                    return Ok(EventPropagation::ConsumedRender);
                                }
                            }
                        }
                    }

                    if let Key::Character(typed_char) = key {
                        // Check keybindings config first
                        let context = self.get_current_context(global_data);
                        let empty_mask = r3bl_tui::ModifierKeysMask {
                            ctrl_key_state: KeyState::NotPressed,
                            shift_key_state: KeyState::NotPressed,
                            alt_key_state: KeyState::NotPressed,
                        };

                        if let Some(action) = global_data.state.keybindings.find_binding(
                            &Key::Character(typed_char),
                            &empty_mask,
                            context,
                        ) {
                            // Found binding - clone action and execute it
                            let action = action.clone();
                            return self.handle_key_binding_action(&action, global_data);
                        }

                        // No binding found - fall back to default behavior
                        match typed_char {
                            'x' => {
                                // 'x': toggle completed state (only when NOT editing)
                                if global_data.state.editing_block_index.is_none() {
                                    event_consumed = true;
                                    // Execute operation via signal system (toggle completed state)
                                    if global_data.state.selected_index < self.element_tree.len() {
                                        let element = &self.element_tree[global_data.state.selected_index];

                                        if let Some(operation) = element.get_operation() {
                                            // Get current data row and clone necessary data
                                            let operation_to_send = if let Some(row) = global_data.state.data.get(global_data.state.selected_index) {
                                                if let Some(id_value) = row.get(&operation.descriptor.id_column) {
                                                    if let Some(id_str) = id_value.as_string() {
                                                        // Get current field value and toggle it
                                                        // Handle both Boolean and Integer (SQLite stores booleans as 0/1)
                                                        if let Some(field_value) = row.get(&get_field_name(operation)) {
                                                            let bool_val = match field_value {
                                                                rusty_knowledge::storage::types::Value::Boolean(b) => *b,
                                                                rusty_knowledge::storage::types::Value::Integer(i) => *i != 0,
                                                                _ => {
                                                                    global_data.state.status_message = format!("Field {} is not a boolean or integer", get_field_name(operation));
                                                                    return Ok(EventPropagation::ConsumedRender);
                                                                }
                                                            };
                                                            Some(AppSignal::ExecuteOperation {
                                                                operation_name: operation.descriptor.name.clone(),
                                                                table: operation.descriptor.table.clone(),
                                                                id_column: operation.descriptor.id_column.clone(),
                                                                id_value: id_str.to_string(),
                                                                field: get_field_name(operation),
                                                                new_value: rusty_knowledge::storage::types::Value::Boolean(!bool_val),
                                                            })
                                                        } else {
                                                                    global_data.state.status_message = format!("Field {} not found in row", get_field_name(operation));
                                                            None
                                                        }
                                                    } else {
                                                                    global_data.state.status_message = format!("ID column {} is not a string", operation.descriptor.id_column);
                                                        None
                                                    }
                                                } else {
                                                                global_data.state.status_message = format!("ID column {} not found", operation.descriptor.id_column);
                                                    None
                                                }
                                            } else {
                                                global_data.state.status_message = format!("No data at index {}", global_data.state.selected_index);
                                                None
                                            };

                                            // Send signal if we have an operation
                                            if let Some(op_signal) = operation_to_send {
                                                send_signal!(
                                                    global_data.main_thread_channel_sender,
                                                    TerminalWindowMainThreadSignal::ApplyAppSignal(op_signal)
                                                );
                                                global_data.state.status_message = "Executing operation...".to_string();
                                            } else {
                                                // Operation found but couldn't build signal - error message already set above
                                            }
                                        } else {
                                            global_data.state.status_message = format!("No operation found for element at index {}", global_data.state.selected_index);
                                        }
                                    } else {
                                        global_data.state.status_message = format!("Selected index {} out of bounds (len: {})", global_data.state.selected_index, self.element_tree.len());
                                    }
                                }
                                // If editing, 'x' is passed through to EditorEngine (inserts 'x' character)
                            }
                            ']' => {
                                event_consumed = true;
                                match global_data.state.execute_operation_on_selected("indent", None) {
                                    Ok(_) => global_data.state.status_message = "Indenting...".to_string(),
                                    Err(e) => global_data.state.status_message = format!("Indent failed: {}", e),
                                }
                            }
                            '[' => {
                                event_consumed = true;
                                match global_data.state.execute_operation_on_selected("outdent", None) {
                                    Ok(_) => global_data.state.status_message = "Outdenting...".to_string(),
                                    Err(e) => global_data.state.status_message = format!("Outdent failed: {}", e),
                                }
                            }
                            _ => {}
                        }
                    }

                    if let Key::SpecialKey(special_key) = key {
                        // Check keybindings config first
                        let context = self.get_current_context(global_data);
                        let empty_mask = r3bl_tui::ModifierKeysMask {
                            ctrl_key_state: KeyState::NotPressed,
                            shift_key_state: KeyState::NotPressed,
                            alt_key_state: KeyState::NotPressed,
                        };

                        if let Some(action) = global_data.state.keybindings.find_binding(
                            &Key::SpecialKey(special_key),
                            &empty_mask,
                            context,
                        ) {
                            // Found binding - clone action and execute it
                            let action = action.clone();
                            return self.handle_key_binding_action(&action, global_data);
                        }

                        // No binding found - fall back to default behavior
                        match special_key {
                            SpecialKey::Up => {
                                // Not editing (editing case handled earlier): move selection and start editing
                                global_data.state.select_previous();
                                return self.start_editing_selected_block(global_data);
                            }
                            SpecialKey::Down => {
                                // Not editing (editing case handled earlier): move selection and start editing
                                global_data.state.select_next();
                                return self.start_editing_selected_block(global_data);
                            }
                            SpecialKey::Enter => {
                                // Enter: start editing if editable_text is selected
                                if global_data.state.editing_block_index.is_none() {
                                    return self.start_editing_selected_block(global_data);
                                }
                            }
                            SpecialKey::Tab => {
                                event_consumed = true;
                                match global_data.state.execute_operation_on_selected("indent", None) {
                                    Ok(_) => global_data.state.status_message = "Indenting...".to_string(),
                                    Err(e) => global_data.state.status_message = format!("Indent failed: {}", e),
                                }
                            }
                            _ => {}
                        }
                    }
                }
                InputEvent::Keyboard(KeyPress::WithModifiers { key, mask }) => {
                    // Check keybindings config first (before handling edit mode)
                    let context = self.get_current_context(global_data);

                    if let Some(action) = global_data.state.keybindings.find_binding(
                        &key,
                        &mask,
                        context,
                    ) {
                        // Found binding - clone action and execute it
                        let action = action.clone();
                        return self.handle_key_binding_action(&action, global_data);
                    }

                    // DEBUG: Log all WithModifiers events when editing
                    if global_data.state.editing_block_index.is_some() {
                        global_data.state.status_message = format!("WithModifiers: key={:?}, shift={:?}, ctrl={:?}",
                            key, mask.shift_key_state, mask.ctrl_key_state);
                    }

                    // Handle edit mode first - forward WithModifiers events to EditorEngine
                    if let Some(_editing_idx) = global_data.state.editing_block_index {
                        if let Some(buffer) = &mut global_data.state.editing_buffer {
                            // Get or create editor engine (reuse across events)
                            if self.editor_engine.is_none() {
                                self.editor_engine = Some(EditorEngine::new(EditorEngineConfig {
                                    multiline_mode: r3bl_tui::LineMode::MultiLine,
                                    syntax_highlight: r3bl_tui::SyntaxHighlightMode::Disable,
                                    edit_mode: r3bl_tui::EditMode::ReadWrite,
                                }));
                            }

                            let editor_engine = self.editor_engine.as_mut().unwrap();

                            // CRITICAL: Set viewport on EditorEngine before processing events
                            if editor_engine.viewport() == Size::default() {
                                editor_engine.current_box.style_adjusted_bounds_size = width(200) + height(50);
                            }

                            // Handle Ctrl+Enter: save and exit edit mode
                            // Shift+Enter: insert newline (if terminal supports it as WithModifiers)
                            // Plain Enter: insert newline (handled by EditorEngine in Plain handler)
                            // Alt+Enter: split block at cursor position
                            if let Key::SpecialKey(SpecialKey::Enter) = key {
                                if mask.alt_key_state == KeyState::Pressed {
                                    // Alt+Enter: split block at cursor
                                    let cursor_offset = match global_data.state.calculate_cursor_offset() {
                                        Ok(offset) => offset,
                                        Err(e) => {
                                            global_data.state.status_message = format!("Split failed: {}", e);
                                            return Ok(EventPropagation::ConsumedRender);
                                        }
                                    };
                                    let block_id = match global_data.state.selected_block_id() {
                                        Some(id) => id,
                                        None => {
                                            global_data.state.status_message = "Split failed: No block selected".to_string();
                                            return Ok(EventPropagation::ConsumedRender);
                                        }
                                    };
                                    let ui_state = rusty_knowledge::api::render_engine::UiState {
                                        cursor_pos: Some(rusty_knowledge::api::render_engine::CursorPosition {
                                            block_id: block_id.clone(),
                                            offset: cursor_offset,
                                        }),
                                        focused_id: Some(block_id),
                                    };
                                    match global_data.state.execute_operation_on_selected("split_block", Some(ui_state)) {
                                        Ok(_) => {
                                            global_data.state.status_message = "Splitting block...".to_string();
                                            // Exit edit mode after split
                                            global_data.state.editing_block_index = None;
                                            global_data.state.editing_buffer = None;
                                            self.editor_engine = None;
                                        }
                                        Err(e) => global_data.state.status_message = format!("Split failed: {}", e),
                                    }
                                    return Ok(EventPropagation::ConsumedRender);
                                } else if mask.ctrl_key_state == KeyState::Pressed {
                                    // Ctrl+Enter: save and exit edit mode
                                    global_data.state.status_message = "Ctrl+Enter - saving".to_string();
                                    // Extract operation info from the EditableText
                                    let operation_info = if let Some(element) = self.element_tree.get(global_data.state.editing_block_index.unwrap()) {
                                        element.find_editable_text()
                                            .and_then(|editable| {
                                                if let UIElement::EditableText { operations, .. } = editable {
                                                    operations.first().map(|op| (
                                                        op.descriptor.name.clone(),
                                                        op.descriptor.table.clone(),
                                                        op.descriptor.id_column.clone(),
                                                        get_field_name(op),
                                                    ))
                                                } else {
                                                    None
                                                }
                                            })
                                    } else {
                                        None
                                    };

                                    // Get content from buffer - join all lines with \n
                                    let lines = buffer.get_lines();
                                    let mut buffer_content = String::new();
                                    let mut i = 0;
                                    loop {
                                        if let Some(line) = lines.get_line_content(row(i)) {
                                            if !buffer_content.is_empty() {
                                                buffer_content.push('\n');
                                            }
                                            buffer_content.push_str(line);
                                            i += 1;
                                        } else {
                                            break;
                                        }
                                    }

                                    // Exit edit mode and clear buffer first (release mutable borrow)
                                    let editing_idx = global_data.state.editing_block_index.unwrap();
                                    global_data.state.editing_block_index = None;
                                    global_data.state.editing_buffer = None;
                                    // Clear editor engine when exiting edit mode
                                    self.editor_engine = None;

                                    // Now we can borrow state.data immutably
                                    if let Some((operation_name, table, id_column, field)) = operation_info {
                                        if let Some(row_data) = global_data.state.data.get(editing_idx) {
                                            if let Some(id_value) = row_data.get(&id_column) {
                                                if let Some(id_str) = id_value.as_string() {
                                                    let new_value = rusty_knowledge::storage::types::Value::String(buffer_content);
                                                    let op_signal = AppSignal::ExecuteOperation {
                                                        operation_name: operation_name.clone(),
                                                        table,
                                                        id_column,
                                                        id_value: id_str.to_string(),
                                                        field,
                                                        new_value,
                                                    };
                                                    send_signal!(
                                                        global_data.main_thread_channel_sender,
                                                        TerminalWindowMainThreadSignal::ApplyAppSignal(op_signal)
                                                    );
                                                    global_data.state.status_message = "Saving...".to_string();
                                                }
                                            }
                                        }
                                    }
                                    return Ok(EventPropagation::ConsumedRender);
                                } else if mask.shift_key_state == KeyState::Pressed {
                                    // Shift+Enter: manually create InsertNewLine event and apply it
                                    // EditorEvent::try_from() only converts Plain Enter, not WithModifiers Enter
                                    global_data.state.status_message = "Shift+Enter detected - inserting newline".to_string();
                                    EditorEvent::apply_editor_event(
                                        editor_engine,
                                        buffer,
                                        EditorEvent::InsertNewLine,
                                        &mut SystemClipboard,
                                    );
                                    return Ok(EventPropagation::ConsumedRender);
                                } else {
                                    // Enter without modifiers in WithModifiers: shouldn't happen, but let EditorEngine handle it
                                    global_data.state.status_message = "Enter (WithModifiers but no modifiers) - passing to EditorEngine".to_string();
                                    // Enter without Shift: save and exit edit mode
                                    // Extract operation info from the EditableText
                                    let operation_info = if let Some(element) = self.element_tree.get(global_data.state.editing_block_index.unwrap()) {
                                        element.find_editable_text()
                                            .and_then(|editable| {
                                                if let UIElement::EditableText { operations, .. } = editable {
                                                    operations.first().map(|op| (
                                                        op.descriptor.name.clone(),
                                                        op.descriptor.table.clone(),
                                                        op.descriptor.id_column.clone(),
                                                        get_field_name(op),
                                                    ))
                                                } else {
                                                    None
                                                }
                                            })
                                    } else {
                                        None
                                    };

                                    // Get content from buffer - join all lines with \n
                                    let lines = buffer.get_lines();
                                    let mut buffer_content = String::new();
                                    let mut i = 0;
                                    loop {
                                        if let Some(line) = lines.get_line_content(row(i)) {
                                            if !buffer_content.is_empty() {
                                                buffer_content.push('\n');
                                            }
                                            buffer_content.push_str(line);
                                            i += 1;
                                        } else {
                                            break;
                                        }
                                    }

                                    // Exit edit mode and clear buffer first (release mutable borrow)
                                    let editing_idx = global_data.state.editing_block_index.unwrap();
                                    global_data.state.editing_block_index = None;
                                    global_data.state.editing_buffer = None;
                                    // Clear editor engine when exiting edit mode
                                    self.editor_engine = None;

                                    // Now we can borrow state.data immutably
                                    if let Some((operation_name, table, id_column, field)) = operation_info {
                                        if let Some(row_data) = global_data.state.data.get(editing_idx) {
                                            if let Some(id_value) = row_data.get(&id_column) {
                                                if let Some(id_str) = id_value.as_string() {
                                                    let new_value = rusty_knowledge::storage::types::Value::String(buffer_content);
                                                    let op_signal = AppSignal::ExecuteOperation {
                                                        operation_name: operation_name.clone(),
                                                        table,
                                                        id_column,
                                                        id_value: id_str.to_string(),
                                                        field,
                                                        new_value,
                                                    };
                                                    send_signal!(
                                                        global_data.main_thread_channel_sender,
                                                        TerminalWindowMainThreadSignal::ApplyAppSignal(op_signal)
                                                    );
                                                    global_data.state.status_message = "Saving...".to_string();
                                                }
                                            }
                                        }
                                    }
                                    return Ok(EventPropagation::ConsumedRender);
                                }
                            } else {
                                // Process other input events through EditorEngine
                                match engine_public_api::apply_event(
                                    buffer,
                                    editor_engine,
                                    input_event.clone(),
                                    &mut SystemClipboard,
                                ) {
                                    Ok(EditorEngineApplyEventResult::Applied) => {
                                        // Event was handled by editor - consume and render
                                        return Ok(EventPropagation::ConsumedRender);
                                    }
                                    Ok(EditorEngineApplyEventResult::NotApplied) => {
                                        // Event not handled by editor - fall through to app-level handlers below
                                    }
                                    Err(e) => {
                                        global_data.state.status_message = format!("Editor error: {}", e);
                                        return Ok(EventPropagation::ConsumedRender);
                                    }
                                }
                            }
                        }
                    }

                    // Handle WithModifiers keys in navigation mode (fallback to default behavior)
                    if let Key::SpecialKey(special_key) = key {
                        match special_key {
                            SpecialKey::Tab if mask.shift_key_state == KeyState::Pressed => {
                                event_consumed = true;
                                match global_data.state.execute_operation_on_selected("outdent", None) {
                                    Ok(_) => global_data.state.status_message = "Outdenting...".to_string(),
                                    Err(e) => global_data.state.status_message = format!("Outdent failed: {}", e),
                                }
                            }
                            SpecialKey::Up if mask.ctrl_key_state == KeyState::Pressed => {
                                event_consumed = true;
                                match global_data.state.execute_operation_on_selected("move_up", None) {
                                    Ok(_) => global_data.state.status_message = "Moving up...".to_string(),
                                    Err(e) => global_data.state.status_message = format!("Move up failed: {}", e),
                                }
                            }
                            SpecialKey::Down if mask.ctrl_key_state == KeyState::Pressed => {
                                event_consumed = true;
                                match global_data.state.execute_operation_on_selected("move_down", None) {
                                    Ok(_) => global_data.state.status_message = "Moving down...".to_string(),
                                    Err(e) => global_data.state.status_message = format!("Move down failed: {}", e),
                                }
                            }
                            SpecialKey::Right if mask.ctrl_key_state == KeyState::Pressed => {
                                event_consumed = true;
                                match global_data.state.execute_operation_on_selected("indent", None) {
                                    Ok(_) => global_data.state.status_message = "Indenting...".to_string(),
                                    Err(e) => global_data.state.status_message = format!("Indent failed: {}", e),
                                }
                            }
                            SpecialKey::Left if mask.ctrl_key_state == KeyState::Pressed => {
                                event_consumed = true;
                                match global_data.state.execute_operation_on_selected("outdent", None) {
                                    Ok(_) => global_data.state.status_message = "Outdenting...".to_string(),
                                    Err(e) => global_data.state.status_message = format!("Outdent failed: {}", e),
                                }
                            }
                            _ => {}
                        }
                    }
                }
                _ => {}
            }

            if event_consumed {
                EventPropagation::ConsumedRender
            } else {
                EventPropagation::Propagate
            }
        });
    }

    fn render(
        &mut self,
        global_data: &mut GlobalData<State, AppSignal>,
        _current_box: FlexBox,
        surface_bounds: SurfaceBounds,
        has_focus: &mut HasFocus,
    ) -> CommonResult<RenderPipeline> {
        throws_with_return!({
            // Rebuild element tree on each render to reflect state changes
            self.rebuild_element_tree(global_data);

            // Check if this component has focus
            let is_focused = has_focus.does_id_have_focus(self.id);

            let mut pipeline = render_pipeline!();

            pipeline.push(ZOrder::Normal, {
                let mut render_ops = r3bl_tui::RenderOpIRVec::new();

            // Use new element tree rendering with focus state
            RenderInterpreter::render_element_tree(
                &self.element_tree,
                &mut render_ops,
                surface_bounds.origin_pos.row_index.as_usize(),
                &global_data.state.data,
                is_focused,
                global_data.state.editing_block_index,
                global_data.state.editing_buffer.as_ref(),
            );

                render_ops
            });

            pipeline
        });
    }
}
