/// Flutter repository wrapper for the Rust DocumentRepository backend.
///
/// This class provides:
/// - Caching layer for efficient block access
/// - Echo suppression to prevent duplicate UI updates from P2P sync
/// - Stream management for change notifications
/// - Lifecycle management for proper resource cleanup
library;

import 'dart:async';
import 'package:flutter/foundation.dart';
import '../src/rust/api/repository.dart' as rust;
import '../src/rust/api/types.dart' as rust;

class RustBlockRepository {
  /// The underlying Rust repository (FFI)
  final rust.RustDocumentRepository _backend;

  /// Cache of blocks by ID for efficient access
  final Map<String, rust.MirrorBlock> _cache = {};

  /// IDs of blocks created/updated locally (for echo suppression)
  final Set<String> _localEditIds = {};

  /// Stream controller for change notifications
  final StreamController<BlockChangeEvent> _changeController =
      StreamController<BlockChangeEvent>.broadcast();

  /// Subscription to the Rust change stream
  StreamSubscription<rust.MirrorBlockChange>? _changeSubscription;

  /// Whether this repository has been disposed
  bool _disposed = false;

  RustBlockRepository._(this._backend);

  /// Create a new document repository and initialize it.
  static Future<RustBlockRepository> createNew(String docId) async {
    final backend = await rust.RustDocumentRepository.createNew(docId: docId);
    final repo = RustBlockRepository._(backend);
    await repo._initialize();
    return repo;
  }

  /// Open an existing document repository and initialize it.
  static Future<RustBlockRepository> openExisting(String docId) async {
    final backend = await rust.RustDocumentRepository.openExisting(
      docId: docId,
    );
    final repo = RustBlockRepository._(backend);
    await repo._initialize();
    return repo;
  }

  /// Initialize the repository by streaming initial state and subscribing to changes.
  Future<void> _initialize() async {
    // Watch changes from the beginning (includes current state as Created events)
    final changeStream = _backend.watchChangesSince(
      position: rust.MirrorStreamPosition.beginning(),
    );

    _changeSubscription = changeStream.listen(
      _handleChange,
      onError: (error) {
        debugPrint('Change stream error: $error');
      },
      onDone: () {
        debugPrint('Change stream closed');
      },
    );
  }

  /// Handle API errors from the Rust backend.
  void _handleApiError(rust.MirrorApiError error) {
    error.when(
      blockNotFound: (id) => debugPrint('Block not found: $id'),
      documentNotFound: (docId) => debugPrint('Document not found: $docId'),
      cyclicMove: (id, target) => debugPrint('Cyclic move: $id -> $target'),
      invalidOperation: (msg) => debugPrint('Invalid operation: $msg'),
      networkError: (msg) => debugPrint('Network error: $msg'),
      internalError: (msg) => debugPrint('Internal error: $msg'),
    );
  }

  /// Handle incoming change events from the Rust backend.
  void _handleChange(rust.MirrorBlockChange change) {
    // Pattern match on the change type using freezed's .when()
    change.when(
      created: (block, origin) {
        // Add to cache
        _cache[block.id] = block;

        // Echo suppression: skip if local and in _localEditIds
        if (origin == rust.MirrorChangeOrigin.local &&
            _localEditIds.contains(block.id)) {
          return;
        }

        // Emit to stream
        _changeController.add(
          BlockCreatedEvent(block, origin == rust.MirrorChangeOrigin.local),
        );
      },
      updated: (id, content, origin) {
        // Update cache
        if (_cache.containsKey(id)) {
          final oldBlock = _cache[id]!;
          _cache[id] = rust.MirrorBlock(
            id: id,
            parentId: oldBlock.parentId,
            content: content,
            children: oldBlock.children,
            metadata: oldBlock.metadata,
          );
        }

        // Echo suppression
        if (origin == rust.MirrorChangeOrigin.local &&
            _localEditIds.contains(id)) {
          return;
        }

        _changeController.add(
          BlockUpdatedEvent(
            id,
            content,
            origin == rust.MirrorChangeOrigin.local,
          ),
        );
      },
      deleted: (id, origin) {
        // Remove from cache
        _cache.remove(id);

        // Echo suppression
        if (origin == rust.MirrorChangeOrigin.local &&
            _localEditIds.contains(id)) {
          return;
        }

        _changeController.add(
          BlockDeletedEvent(id, origin == rust.MirrorChangeOrigin.local),
        );
      },
      moved: (id, newParent, after, origin) {
        // Update cache with new parent
        if (_cache.containsKey(id)) {
          final oldBlock = _cache[id]!;
          _cache[id] = rust.MirrorBlock(
            id: id,
            parentId: newParent,
            content: oldBlock.content,
            children: oldBlock.children,
            metadata: oldBlock.metadata,
          );
        }

        // Echo suppression
        if (origin == rust.MirrorChangeOrigin.local &&
            _localEditIds.contains(id)) {
          return;
        }

        _changeController.add(
          BlockMovedEvent(
            id,
            newParent,
            after,
            origin == rust.MirrorChangeOrigin.local,
          ),
        );
      },
    );
  }

  /// Stream of block changes (after echo suppression and caching).
  Stream<BlockChangeEvent> get changes => _changeController.stream;

  /// Get a block by ID (cache-first strategy).
  Future<rust.MirrorBlock?> getBlock(String id) async {
    // Check cache first
    if (_cache.containsKey(id)) {
      return _cache[id]!;
    }

    // Fetch from Rust and cache
    try {
      final block = await _backend.getBlock(id: id);
      _cache[id] = block;
      return block;
    } on rust.MirrorApiError catch (e) {
      _handleApiError(e);
      return null;
    } catch (e) {
      debugPrint('Unexpected error in getBlock: $e');
      return null;
    }
  }

  /// Get multiple blocks by ID (batch operation).
  Future<List<rust.MirrorBlock>> getBlocks(List<String> ids) async {
    final List<rust.MirrorBlock> result = [];
    final List<String> missingIds = [];

    // Collect cached blocks and identify missing ones
    for (final id in ids) {
      if (_cache.containsKey(id)) {
        result.add(_cache[id]!);
      } else {
        missingIds.add(id);
      }
    }

    // Batch fetch missing blocks
    if (missingIds.isNotEmpty) {
      try {
        final fetched = await _backend.getBlocks(ids: missingIds);
        for (final block in fetched) {
          final blockId = block.id;
          _cache[blockId] = block;
          result.add(block);
        }
      } on rust.MirrorApiError catch (e) {
        _handleApiError(e);
      } catch (e) {
        debugPrint('Unexpected error in getBlocks: $e');
      }
    }

    return result;
  }

  /// Create a new block.
  Future<rust.MirrorBlock?> createBlock({
    required String content,
    String? parentId,
    String? id,
  }) async {
    try {
      // Create in Rust
      final block = await _backend.createBlock(
        parentId: parentId,
        content: content,
        id: id,
      );

      // Mark as local edit for echo suppression
      final blockId = block.id;
      _localEditIds.add(blockId);

      // Update cache
      _cache[blockId] = block;

      // Clean up echo suppression marker after a delay
      Future.delayed(const Duration(seconds: 2), () {
        _localEditIds.remove(blockId);
      });

      return block;
    } on rust.MirrorApiError catch (e) {
      _handleApiError(e);
      return null;
    } catch (e) {
      debugPrint('Unexpected error in createBlock: $e');
      return null;
    }
  }

  /// Update block content.
  Future<void> updateBlock(String id, String content) async {
    try {
      // Mark as local edit
      _localEditIds.add(id);

      // Update in Rust
      await _backend.updateBlock(id: id, content: content);

      // Invalidate cache (will be refreshed on next access or via change stream)
      _cache.remove(id);

      // Clean up echo suppression marker
      Future.delayed(const Duration(seconds: 2), () {
        _localEditIds.remove(id);
      });
    } on rust.MirrorApiError catch (e) {
      _handleApiError(e);
      _localEditIds.remove(id);
    } catch (e) {
      debugPrint('Unexpected error in updateBlock: $e');
      _localEditIds.remove(id);
    }
  }

  /// Delete a block.
  Future<void> deleteBlock(String id) async {
    try {
      // Mark as local edit
      _localEditIds.add(id);

      // Delete in Rust
      await _backend.deleteBlock(id: id);

      // Remove from cache
      _cache.remove(id);

      // Clean up echo suppression marker
      Future.delayed(const Duration(seconds: 2), () {
        _localEditIds.remove(id);
      });
    } on rust.MirrorApiError catch (e) {
      _handleApiError(e);
      _localEditIds.remove(id);
    } catch (e) {
      debugPrint('Unexpected error in deleteBlock: $e');
      _localEditIds.remove(id);
    }
  }

  /// Move block to new parent and position.
  Future<void> moveBlock({
    required String id,
    String? newParent,
    String? after,
  }) async {
    try {
      // Mark as local edit
      _localEditIds.add(id);

      // Move in Rust
      await _backend.moveBlock(id: id, newParent: newParent, after: after);

      // Invalidate cache
      _cache.remove(id);

      // Clean up echo suppression marker
      Future.delayed(const Duration(seconds: 2), () {
        _localEditIds.remove(id);
      });
    } on rust.MirrorApiError catch (e) {
      _handleApiError(e);
      _localEditIds.remove(id);
    } catch (e) {
      debugPrint('Unexpected error in moveBlock: $e');
      _localEditIds.remove(id);
    }
  }

  /// Get root-level block IDs.
  Future<List<String>> getRootBlocks() async {
    try {
      return await _backend.getRootBlocks();
    } on rust.MirrorApiError catch (e) {
      _handleApiError(e);
      return [];
    } catch (e) {
      debugPrint('Unexpected error in getRootBlocks: $e');
      return [];
    }
  }

  /// List children of a block.
  Future<List<String>> listChildren(String parentId) async {
    try {
      return await _backend.listChildren(parentId: parentId);
    } on rust.MirrorApiError catch (e) {
      _handleApiError(e);
      return [];
    } catch (e) {
      debugPrint('Unexpected error in listChildren: $e');
      return [];
    }
  }

  /// Get this node's P2P identifier.
  Future<String> getNodeId() async {
    return await _backend.getNodeId();
  }

  /// Connect to a peer for P2P synchronization.
  Future<void> connectToPeer(String peerNodeId) async {
    try {
      await _backend.connectToPeer(peerNodeId: peerNodeId);
    } on rust.MirrorApiError catch (e) {
      _handleApiError(e);
    } catch (e) {
      debugPrint('Unexpected error in connectToPeer: $e');
    }
  }

  /// Start accepting incoming P2P connections.
  Future<void> acceptConnections() async {
    try {
      await _backend.acceptConnections();
    } on rust.MirrorApiError catch (e) {
      _handleApiError(e);
    } catch (e) {
      debugPrint('Unexpected error in acceptConnections: $e');
    }
  }

  /// Dispose of resources and clean up.
  Future<void> dispose() async {
    if (_disposed) return;
    _disposed = true;

    // Cancel change subscription
    await _changeSubscription?.cancel();

    // Close change stream controller
    await _changeController.close();

    // Dispose Rust backend
    await _backend.dispose();

    // Clear cache
    _cache.clear();
    _localEditIds.clear();
  }
}

/// Event representing a block change.
///
/// TODO: Implement proper change event types once we figure out
/// how to extract BlockChange variants from the opaque Result type.
abstract class BlockChangeEvent {
  const BlockChangeEvent();
}

class BlockCreatedEvent extends BlockChangeEvent {
  final rust.MirrorBlock block;
  final bool isLocal;

  const BlockCreatedEvent(this.block, this.isLocal);
}

class BlockUpdatedEvent extends BlockChangeEvent {
  final String id;
  final String content;
  final bool isLocal;

  const BlockUpdatedEvent(this.id, this.content, this.isLocal);
}

class BlockDeletedEvent extends BlockChangeEvent {
  final String id;
  final bool isLocal;

  const BlockDeletedEvent(this.id, this.isLocal);
}

class BlockMovedEvent extends BlockChangeEvent {
  final String id;
  final String? newParent;
  final String? after;
  final bool isLocal;

  const BlockMovedEvent(this.id, this.newParent, this.after, this.isLocal);
}
