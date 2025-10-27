/// Adapter that implements OutlinerRepository interface using RustBlockRepository.
///
/// This adapter bridges between:
/// - outliner-flutter's hierarchical Block model (children as nested objects)
/// - rusty-knowledge's flat MirrorBlock structure (children as ID lists)
library;

import 'package:outliner_view/outliner_view.dart' as outliner;
import 'package:flutter/foundation.dart';
import '../src/rust/api/types.dart' as rust;
import 'rust_block_repository.dart';

class RustyOutlinerRepository implements outliner.OutlinerRepository {
  final RustBlockRepository _rustRepo;
  final String _rootBlockId;

  /// Tracks collapsed state locally (not persisted to backend)
  final Map<String, bool> _collapsedState = {};

  RustyOutlinerRepository(this._rustRepo, this._rootBlockId);

  /// Convert a flat MirrorBlock into a hierarchical outliner Block
  Future<outliner.Block> _convertToOutlinerBlock(
    rust.MirrorBlock mirrorBlock,
  ) async {
    // Build children recursively
    final List<outliner.Block> children = [];
    for (final childId in mirrorBlock.children) {
      final childMirror = await _rustRepo.getBlock(childId);
      if (childMirror != null) {
        children.add(await _convertToOutlinerBlock(childMirror));
      }
    }

    return outliner.Block(
      id: mirrorBlock.id,
      content: mirrorBlock.content,
      children: children,
      isCollapsed: _collapsedState[mirrorBlock.id] ?? false,
      createdAt: DateTime.fromMillisecondsSinceEpoch(
        mirrorBlock.metadata.createdAt.toInt(),
      ),
      updatedAt: DateTime.fromMillisecondsSinceEpoch(
        mirrorBlock.metadata.updatedAt.toInt(),
      ),
    );
  }

  @override
  Future<outliner.Block> getRootBlock() async {
    final mirrorBlock = await _rustRepo.getBlock(_rootBlockId);
    if (mirrorBlock == null) {
      throw Exception('Root block not found: $_rootBlockId');
    }
    return await _convertToOutlinerBlock(mirrorBlock);
  }

  @override
  Future<outliner.Block?> findBlockById(String blockId) async {
    try {
      final mirrorBlock = await _rustRepo.getBlock(blockId);
      if (mirrorBlock == null) return null;
      return await _convertToOutlinerBlock(mirrorBlock);
    } catch (e) {
      debugPrint('Error finding block by id: $e');
      return null;
    }
  }

  @override
  Future<String> findParentId(String blockId) async {
    try {
      final mirrorBlock = await _rustRepo.getBlock(blockId);
      return mirrorBlock?.parentId;
    } catch (e) {
      debugPrint('Error finding parent id: $e');
      return null;
    }
  }

  @override
  Future<int> findBlockIndex(String blockId) async {
    try {
      final mirrorBlock = await _rustRepo.getBlock(blockId);
      if (mirrorBlock == null) return -1;

      // Child block - find index in parent's children
      final parent = await _rustRepo.getBlock(mirrorBlock.parentId);
      if (parent == null) return -1;
      return parent.children.indexOf(blockId);
    } catch (e) {
      debugPrint('Error finding block index: $e');
      return -1;
    }
  }

  @override
  Future<int> getTotalBlocks() async {
    try {
      // Count all blocks recursively
      final rootBlocks = await getRootBlocks();
      int total = 0;
      for (final block in rootBlocks) {
        total += block.totalBlocks;
      }
      return total;
    } catch (e) {
      debugPrint('Error getting total blocks: $e');
      return 0;
    }
  }

  @override
  Future<void> addRootBlock(outliner.Block block) async {
    try {
      await _rustRepo.createBlock(
        content: block.content,
        parentId: null,
        id: block.id,
      );
    } catch (e) {
      debugPrint('Error adding root block: $e');
    }
  }

  @override
  Future<void> insertRootBlock(int index, outliner.Block block) async {
    try {
      // Find the block that should come before this one
      final rootIds = await _rustRepo.getRootBlocks();
      final String? after = (index > 0 && index <= rootIds.length)
          ? rootIds[index - 1]
          : null;

      // Create block with positioning
      final created = await _rustRepo.createBlock(
        content: block.content,
        parentId: null,
        id: block.id,
      );

      if (created != null && after != null) {
        // Move to correct position
        await _rustRepo.moveBlock(
          id: created.id,
          newParent: null,
          after: after,
        );
      }
    } catch (e) {
      debugPrint('Error inserting root block: $e');
    }
  }

  @override
  Future<void> removeRootBlock(outliner.Block block) async {
    try {
      await _rustRepo.deleteBlock(block.id);
      _collapsedState.remove(block.id);
    } catch (e) {
      debugPrint('Error removing root block: $e');
    }
  }

  @override
  Future<void> updateBlock(String blockId, String content) async {
    try {
      await _rustRepo.updateBlock(blockId, content);
    } catch (e) {
      debugPrint('Error updating block: $e');
    }
  }

  @override
  Future<void> toggleBlockCollapse(String blockId) async {
    // This is UI-only state, not persisted to backend
    _collapsedState[blockId] = !(_collapsedState[blockId] ?? false);
  }

  @override
  Future<void> addChildBlock(String parentId, outliner.Block child) async {
    try {
      await _rustRepo.createBlock(
        content: child.content,
        parentId: parentId,
        id: child.id,
      );
    } catch (e) {
      debugPrint('Error adding child block: $e');
    }
  }

  @override
  Future<void> removeBlock(String blockId) async {
    try {
      await _rustRepo.deleteBlock(blockId);
      _collapsedState.remove(blockId);
    } catch (e) {
      debugPrint('Error removing block: $e');
    }
  }

  @override
  Future<void> moveBlock(
    String blockId,
    String? newParentId,
    int newIndex,
  ) async {
    try {
      // Find the block that should come before this one
      List<String> siblingIds;
      if (newParentId == null) {
        siblingIds = await _rustRepo.getRootBlocks();
      } else {
        siblingIds = await _rustRepo.listChildren(newParentId);
      }

      // Calculate the 'after' anchor
      final String? after = (newIndex > 0 && newIndex <= siblingIds.length)
          ? siblingIds[newIndex - 1]
          : null;

      await _rustRepo.moveBlock(
        id: blockId,
        newParent: newParentId,
        after: after,
      );
    } catch (e) {
      debugPrint('Error moving block: $e');
    }
  }

  @override
  Future<void> indentBlock(String blockId) async {
    try {
      // Find the block's current position
      final mirrorBlock = await _rustRepo.getBlock(blockId);
      if (mirrorBlock == null) return;

      // Get siblings
      List<String> siblingIds;
      siblingIds = await _rustRepo.listChildren(mirrorBlock.parentId);

      final currentIndex = siblingIds.indexOf(blockId);
      if (currentIndex <= 0) return; // Can't indent if first child

      // The new parent is the previous sibling
      final newParentId = siblingIds[currentIndex - 1];

      // Move to the end of the new parent's children
      await _rustRepo.moveBlock(
        id: blockId,
        newParent: newParentId,
        after: null, // null means "append to end"
      );
    } catch (e) {
      debugPrint('Error indenting block: $e');
    }
  }

  @override
  Future<void> outdentBlock(String blockId) async {
    try {
      // Find the block and its parent
      final mirrorBlock = await _rustRepo.getBlock(blockId);
      if (mirrorBlock == null) {
        return; // Already at root
      }

      final parent = await _rustRepo.getBlock(mirrorBlock.parentId);
      if (parent == null) return;

      // Move to parent's level, right after the parent
      await _rustRepo.moveBlock(
        id: blockId,
        newParent: parent.parentId,
        after: parent.id,
      );
    } catch (e) {
      debugPrint('Error outdenting block: $e');
    }
  }

  @override
  Future<String> splitBlock(String blockId, int cursorPosition) async {
      // Get the original block
      final mirrorBlock = await _rustRepo.getBlock(blockId);
      if (mirrorBlock == null) return;

      final content = mirrorBlock.content;

      // Split content at cursor position
      final beforeCursor = content.substring(0, cursorPosition);
      final afterCursor = content.substring(cursorPosition);

      // Update original block with content before cursor
      await _rustRepo.updateBlock(blockId, beforeCursor);

      // Create new block with content after cursor
      final newBlock = await _rustRepo.createBlock(
        content: afterCursor,
        parentId: mirrorBlock.parentId,
      );

      if (newBlock != null) {
        // Position the new block right after the original
        await _rustRepo.moveBlock(
          id: newBlock.id,
          newParent: mirrorBlock.parentId,
          after: blockId,
        );
      }
    throw new UnimplementedError("Must return new block ID");
  }
  Future<String?> findNextVisibleBlock(String blockId) {
    throw UnimplementedError();
  }

  Future<String?> findPreviousVisibleBlock(String blockId) {
    throw UnimplementedError();
  }

}
