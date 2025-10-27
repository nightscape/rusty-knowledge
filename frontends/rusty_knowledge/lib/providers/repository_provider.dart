/// Riverpod providers for the document repository and related state.
library;

import 'dart:async' show unawaited;
import 'package:flutter_riverpod/flutter_riverpod.dart';
import '../data/rust_block_repository.dart';

/// Provider for the current document ID.
///
/// Change this to open a different document. When changed, the repository
/// provider will automatically dispose the old repository and create a new one.
final documentIdProvider = StateProvider<String>((ref) => 'default');

/// Provider for the document repository instance.
///
/// Automatically creates and initializes a repository for the current document ID.
/// Properly disposes of the repository when no longer needed or when document ID changes.
final repositoryProvider = FutureProvider<RustBlockRepository>((ref) async {
  final docId = ref.watch(documentIdProvider);

  // Create and initialize repository
  final repository = await RustBlockRepository.createNew(docId);

  // Dispose when provider is disposed
  ref.onDispose(() {
    unawaited(repository.dispose());
  });

  return repository;
});

/// Provider for P2P connection status.
///
/// Tracks whether we're connected to any peers.
/// TODO: Implement actual connection tracking via backend events.
final connectionStatusProvider = StateProvider<ConnectionStatus>((ref) {
  return ConnectionStatus.offline;
});

/// Provider for the node ID.
///
/// Fetches the node ID from the repository once it's initialized.
final nodeIdProvider = FutureProvider<String>((ref) async {
  final repository = await ref.watch(repositoryProvider.future);
  return repository.getNodeId();
});

/// Provider for root block IDs.
///
/// Loads the root-level blocks from the repository.
final rootBlocksProvider = FutureProvider<List<String>>((ref) async {
  final repository = await ref.watch(repositoryProvider.future);
  return repository.getRootBlocks();
});

/// Provider for a specific block by ID.
///
/// This is a family provider that creates a separate provider for each block ID.
final blockProvider = FutureProvider.family<dynamic, String>((ref, id) async {
  final repository = await ref.watch(repositoryProvider.future);
  return repository.getBlock(id);
});

/// P2P connection status enum.
enum ConnectionStatus {
  /// Not connected to any peers
  offline,

  /// Connecting to a peer
  connecting,

  /// Connected to at least one peer
  online,

  /// Connection error
  error,
}
