/// Custom bullet builder for LogSeq-style bullets.
library;

import 'package:flutter/material.dart';
import '../../data/rust_block_ops.dart' show RustBlock;

/// Build the bullet widget for a block (LogSeq-style).
///
/// This builder is called to render the bullet point/expand/collapse indicator
/// for each block. It receives:
/// - [context]: The build context
/// - [block]: The rust Block to render the bullet for
/// - [hasChildren]: Whether the block has children
/// - [isCollapsed]: Whether the block is currently collapsed
/// - [onToggle]: Callback to toggle the expand/collapse state
///
/// Returns a widget that displays the bullet and handles collapse toggling.
Widget buildBullet(
  BuildContext context,
  RustBlock block,
  bool hasChildren,
  bool isCollapsed,
  VoidCallback? onToggle,
) {
  final theme = Theme.of(context);

  return GestureDetector(
    onTap: hasChildren ? onToggle : null,
    behavior: HitTestBehavior.opaque,
    child: SizedBox(
      width: 20,
      height: 20,
      child: hasChildren
          ? _buildExpandableIcon(theme, isCollapsed)
          : _buildSimpleBullet(theme),
    ),
  );
}

/// Build the expandable/collapsible icon for blocks with children.
Widget _buildExpandableIcon(ThemeData theme, bool isCollapsed) {
  return Icon(
    isCollapsed ? Icons.chevron_right : Icons.expand_more,
    size: 20,
    color: theme.iconTheme.color ?? Colors.grey,
  );
}

/// Build a simple bullet point for blocks without children (LogSeq-style).
Widget _buildSimpleBullet(ThemeData theme) {
  return Container(
    width: 8,
    height: 8,
    margin: const EdgeInsets.all(6),
    decoration: BoxDecoration(
      shape: BoxShape.circle,
      border: Border.all(
        color: theme.primaryColor.withValues(alpha: 0.6),
        width: 2,
      ),
    ),
  );
}
