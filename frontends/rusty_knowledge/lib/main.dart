import 'package:flutter/material.dart';
import 'package:hooks_riverpod/hooks_riverpod.dart';
import 'package:outliner_view/outliner_view.dart';
import 'src/rust/frb_generated.dart';
import 'data/rust_block_repository.dart';
import 'data/outliner_adapter.dart';

Future<void> main() async {
  // Ensure Flutter bindings are initialized
  WidgetsFlutterBinding.ensureInitialized();

  // Initialize the Rust library
  await RustLib.init();

  // Initialize the Rust repository
  final rustRepo = await RustBlockRepository.createNew('default');
  final outlinerRepo = RustyOutlinerRepository(rustRepo, "THE_ROOT_BLOCK_ID"); // TODO: Replace with actual root block ID

  runApp(
    ProviderScope(
      overrides: [
        // Override the outliner repository with our initialized Rust backend
        outlinerRepositoryProvider.overrideWithValue(outlinerRepo),
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
              return FutureBuilder<int>(
                future: ref.read(outlinerProvider.notifier).totalBlocks,
                builder: (context, snapshot) {
                  final count = snapshot.data ?? 0;
                  return Padding(
                    padding: const EdgeInsets.only(right: 16),
                    child: Center(
                      child: Text(
                        '$count blocks',
                        style: Theme.of(context).textTheme.bodyMedium,
                      ),
                    ),
                  );
                },
              );
            },
          ),
        ],
      ),
      body: const OutlinerListView(),
    );
  }
}
