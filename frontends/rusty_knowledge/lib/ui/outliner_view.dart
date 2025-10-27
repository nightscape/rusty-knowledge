/// Main outliner view widget for displaying and editing hierarchical blocks.
library;

import 'package:flutter/material.dart';
import 'package:hooks_riverpod/hooks_riverpod.dart';
import 'package:outliner_view/outliner_view.dart';
import 'widgets/block_builder.dart';
import 'widgets/bullet_builder.dart';

class OutlinerView extends HookConsumerWidget {
  const OutlinerView({super.key});

  @override
  Widget build(BuildContext context, WidgetRef ref) {
    final theme = Theme.of(context);

    return OutlinerListView(
      config: OutlinerConfig(
        keyboardShortcutsEnabled: true,
        blockStyle: BlockStyle(
          indentWidth: 24.0,
          textStyle: theme.textTheme.bodyLarge ?? const TextStyle(),
          emptyTextStyle: TextStyle(
            color: theme.hintColor,
            fontStyle: FontStyle.italic,
          ),
          editingTextStyle: theme.textTheme.bodyLarge ?? const TextStyle(),
          bulletColor: theme.primaryColor,
          contentPadding: const EdgeInsets.symmetric(
            vertical: 4.0,
            horizontal: 8.0,
          ),
        ),
      ),
      blockBuilder: (context, block) {
        return buildBlockContent(context, block, false);
      },
      bulletBuilder: (context, block, hasChildren, isCollapsed, onToggle) {
        return buildBullet(context, block, hasChildren, isCollapsed, onToggle);
      },
      loadingBuilder: (context) {
        return const Center(
          child: Padding(
            padding: EdgeInsets.all(32.0),
            child: CircularProgressIndicator(),
          ),
        );
      },
      errorBuilder: (context, error, retry) {
        return Center(
          child: Padding(
            padding: const EdgeInsets.all(32.0),
            child: Column(
              mainAxisSize: MainAxisSize.min,
              children: [
                const Icon(Icons.error_outline, size: 48, color: Colors.red),
                const SizedBox(height: 16),
                Text('Error: $error'),
                const SizedBox(height: 16),
                ElevatedButton(onPressed: retry, child: const Text('Retry')),
              ],
            ),
          ),
        );
      },
      emptyBuilder: (context, onAddBlock) {
        return Center(
          child: Padding(
            padding: const EdgeInsets.all(32.0),
            child: Column(
              mainAxisSize: MainAxisSize.min,
              children: [
                Icon(Icons.note_add_outlined, size: 64, color: theme.hintColor),
                const SizedBox(height: 16),
                Text(
                  'No blocks yet',
                  style: TextStyle(fontSize: 18, color: theme.hintColor),
                ),
                const SizedBox(height: 8),
                Text(
                  'Tap the + button to create your first block',
                  style: TextStyle(color: theme.hintColor),
                ),
                const SizedBox(height: 16),
                ElevatedButton.icon(
                  onPressed: onAddBlock,
                  icon: const Icon(Icons.add),
                  label: const Text('Add Block'),
                ),
              ],
            ),
          ),
        );
      },
    );
  }
}
