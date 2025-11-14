import 'package:flutter/material.dart';
import 'package:hooks_riverpod/hooks_riverpod.dart';
import 'package:outliner_view/outliner_view.dart' show BlockOps;
import 'src/rust/frb_generated.dart' as frb;
import 'data/rust_block_ops.dart';
import 'providers/repository_provider.dart';
import 'ui/outliner_view.dart';

Future<void> main() async {
  // Ensure Flutter bindings are initialized
  WidgetsFlutterBinding.ensureInitialized();

  // Initialize the Rust library
  await frb.RustLib.init();

  // Initialize RustBlockOps
  final blockOps = await RustBlockOps.createNew('default');

  runApp(
    ProviderScope(
      overrides: [
        // Override the block ops provider with our pre-initialized instance
        blockOpsProvider.overrideWith((ref) async {
          return blockOps as BlockOps<RustBlock>;
        }),
      ],
      child: const MyApp(),
    ),
  );
}

class MyApp extends StatelessWidget {
  const MyApp({super.key});

  @override
  Widget build(BuildContext context) {
    return MaterialApp(
      title: 'Rusty Knowledge',
      debugShowCheckedModeBanner: false,
      theme: ThemeData(
        colorScheme: ColorScheme.fromSeed(seedColor: Colors.blue),
        useMaterial3: true,
      ),
      home: const MainScreen(),
    );
  }
}

class MainScreen extends HookConsumerWidget {
  const MainScreen({super.key});

  @override
  Widget build(BuildContext context, WidgetRef ref) {
    return Scaffold(
      appBar: AppBar(
        title: const Text('Rusty Knowledge'),
        actions: [
          const SizedBox(width: 8),
          Consumer(
            builder: (context, ref, _) {
              final opsAsync = ref.watch(blockOpsProvider);
              return opsAsync.when(
                data: (ops) {
                  // Count all blocks in the cache (simplified - no recursive counting)
                  final topLevel = ops.getTopLevelBlocks();
                  return Padding(
                    padding: const EdgeInsets.only(right: 16),
                    child: Center(
                      child: Text(
                        '${topLevel.length} top-level blocks',
                        style: Theme.of(context).textTheme.bodyMedium,
                      ),
                    ),
                  );
                },
                loading: () => const Padding(
                  padding: EdgeInsets.only(right: 16),
                  child: SizedBox(
                    width: 16,
                    height: 16,
                    child: CircularProgressIndicator(strokeWidth: 2),
                  ),
                ),
                error: (_, __) => const SizedBox.shrink(),
              );
            },
          ),
        ],
      ),
      body: const OutlinerView(),
    );
  }
}
