/// Riverpod provider overrides for the outliner.
///
/// This file overrides the outlinerRepositoryProvider from outliner-flutter
/// to use our custom RustyOutlinerRepository that wraps the Rust backend.
library;

import 'package:flutter_riverpod/flutter_riverpod.dart';
import 'package:outliner_view/outliner_view.dart';
import '../data/outliner_adapter.dart';
import 'repository_provider.dart';

/// Override for the outliner repository provider.
///
/// This replaces the default in-memory repository with our Rust-backed implementation.
/// Use this in your ProviderScope overrides:
///
/// ```dart
/// ProviderScope(
///   overrides: [
///     outlinerRepositoryProvider.overrideWithProvider(rustyOutlinerRepositoryProvider),
///   ],
///   child: MyApp(),
/// )
/// ```
final rustyOutlinerRepositoryProvider = FutureProvider<OutlinerRepository>((
  ref,
) async {
  final rustRepo = await ref.watch(repositoryProvider.future);
  return RustyOutlinerRepository(rustRepo);
});
