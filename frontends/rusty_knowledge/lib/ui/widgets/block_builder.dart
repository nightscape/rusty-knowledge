/// Custom block builder for rendering block content.
library;

import 'package:flutter/material.dart';
import 'package:outliner_view/outliner_view.dart' show Block;

/// Build the content widget for a block.
///
/// This builder is called for each block in the outliner. It receives:
/// - [context]: The build context
/// - [block]: The block to render
/// - [isEditing]: Whether the block is currently being edited
///
/// Returns a widget that displays the block's content.
Widget buildBlockContent(BuildContext context, Block block, bool isEditing) {
  final theme = Theme.of(context);

  if (isEditing) {
    // When editing, return a TextField for inline editing
    return TextField(
      autofocus: true,
      controller: TextEditingController(text: block.content)
        ..selection = TextSelection.collapsed(offset: block.content.length),
      style: theme.textTheme.bodyLarge,
      decoration: const InputDecoration(
        border: InputBorder.none,
        isDense: true,
        contentPadding: EdgeInsets.symmetric(horizontal: 4.0),
      ),
      maxLines: null,
      keyboardType: TextInputType.multiline,
    );
  } else {
    // When not editing, display as selectable text
    return SelectableText(
      block.content.isEmpty ? 'Empty block' : block.content,
      style: block.content.isEmpty
          ? theme.textTheme.bodyLarge?.copyWith(
              color: theme.hintColor,
              fontStyle: FontStyle.italic,
            )
          : theme.textTheme.bodyLarge,
    );
  }
}
