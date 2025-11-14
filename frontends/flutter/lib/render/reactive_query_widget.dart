import 'package:flutter/material.dart';
import '../src/rust/third_party/query_render/types.dart';
import 'render_interpreter.dart';

/// Event type for CDC (Change Data Capture) streaming.
///
/// TODO Phase 4.1: Define these in Rust and expose via FRB.
/// For now, using Dart-side definitions as placeholder.
enum RowEventType { added, updated, removed }

class RowEvent {
  final RowEventType type;
  final String rowId;
  final Map<String, dynamic>? data;

  const RowEvent({
    required this.type,
    required this.rowId,
    this.data,
  });
}

/// Widget that renders a PRQL query with reactive updates via CDC streaming.
///
/// This is the main entry point for rendering PRQL queries in Flutter.
/// It handles:
/// - Streaming CDC events from Rust
/// - Maintaining keyed widget cache for efficient updates
/// - Building UI from RenderSpec using RenderInterpreter
///
/// Usage:
/// ```dart
/// ReactiveQueryWidget(
///   sql: "SELECT * FROM blocks WHERE parent_id = ?",
///   params: {"parent_id": "root"},
///   renderSpec: spec,
///   onOperation: (name, params) => executeOperation(name, params),
/// )
/// ```
class ReactiveQueryWidget extends StatefulWidget {
  /// SQL query to execute (compiled from PRQL).
  final String sql;

  /// Query parameters.
  final Map<String, dynamic> params;

  /// Render specification (AST root).
  final RenderSpec renderSpec;

  /// Callback for executing operations (indent, outdent, etc.).
  final Future<void> Function(String operationName, Map<String, dynamic> params)? onOperation;

  const ReactiveQueryWidget({
    super.key,
    required this.sql,
    required this.params,
    required this.renderSpec,
    this.onOperation,
  });

  @override
  State<ReactiveQueryWidget> createState() => _ReactiveQueryWidgetState();
}

class _ReactiveQueryWidgetState extends State<ReactiveQueryWidget> {
  /// Cached row data keyed by row ID (from `data.get("id")`, NOT ROWID).
  /// See Phase 1.3 documentation for critical keying requirements.
  final Map<String, Map<String, dynamic>> _rowCache = {};

  /// Sorted list of row IDs for stable ordering.
  final List<String> _rowOrder = [];

  /// Stream of CDC events from Rust.
  /// TODO Phase 4.1: Wire up to Rust FFI stream.
  Stream<RowEvent>? _cdcStream;

  /// Render interpreter for building widgets from AST.
  final RenderInterpreter _interpreter = RenderInterpreter();

  @override
  void initState() {
    super.initState();
    _initializeStream();
  }

  @override
  void didUpdateWidget(ReactiveQueryWidget oldWidget) {
    super.didUpdateWidget(oldWidget);
    if (oldWidget.sql != widget.sql || oldWidget.params != widget.params) {
      _initializeStream();
    }
  }

  /// Initialize CDC stream from Rust.
  /// TODO Phase 4.1: Replace with actual Rust FFI call.
  void _initializeStream() {
    // _cdcStream = watchQuery(widget.sql, widget.params);

    // Placeholder: Empty stream for now
    _cdcStream = Stream<RowEvent>.empty();
  }

  @override
  Widget build(BuildContext context) {
    return StreamBuilder<RowEvent>(
      stream: _cdcStream,
      builder: (context, snapshot) {
        if (snapshot.hasError) {
          return _buildError(snapshot.error!);
        }

        // Process CDC event if present
        if (snapshot.hasData) {
          _processCdcEvent(snapshot.data!);
        }

        // Build UI from cached data
        if (_rowCache.isEmpty) {
          return _buildEmpty();
        }

        return _buildFromCache();
      },
    );
  }

  /// Process incoming CDC event and update cache.
  void _processCdcEvent(RowEvent event) {
    setState(() {
      switch (event.type) {
        case RowEventType.added:
          assert(event.data != null, 'Added event must have data');
          _rowCache[event.rowId] = event.data!;
          if (!_rowOrder.contains(event.rowId)) {
            _rowOrder.add(event.rowId);
          }
          break;

        case RowEventType.updated:
          assert(event.data != null, 'Updated event must have data');
          assert(_rowCache.containsKey(event.rowId),
              'Updated event for non-existent row ${event.rowId}');
          _rowCache[event.rowId] = event.data!;
          break;

        case RowEventType.removed:
          _rowCache.remove(event.rowId);
          _rowOrder.remove(event.rowId);
          break;
      }
    });
  }

  /// Build UI from cached row data.
  Widget _buildFromCache() {
    // Get the root expression to determine how to render
    final rootExpr = widget.renderSpec.root;

    // If root is a list() function, build ListView
    return rootExpr.when(
      functionCall: (name, args) {
        if (name == 'list') {
          return _buildListView(args);
        }
        // For non-list roots, render single item
        return _buildSingleItem(rootExpr);
      },
      // For non-function roots, render single item
      columnRef: (_) => _buildSingleItem(rootExpr),
      literal: (_) => _buildSingleItem(rootExpr),
      binaryOp: (_, __, ___) => _buildSingleItem(rootExpr),
      array: (_) => _buildSingleItem(rootExpr),
      object: (_) => _buildSingleItem(rootExpr),
    );
  }

  /// Build ListView with virtualization and keyed widgets.
  Widget _buildListView(List<Arg> listArgs) {
    // Extract item template from list args
    final itemExpr = listArgs
        .firstWhere(
          (arg) => arg.name == 'item',
          orElse: () => throw ArgumentError('list() requires "item" argument'),
        )
        .value;

    return ListView.builder(
      itemCount: _rowOrder.length,
      itemBuilder: (context, index) {
        final rowId = _rowOrder[index];
        final rowData = _rowCache[rowId]!;

        // Use entity ID as key (NOT index, NOT ROWID)
        // See Phase 1.3 documentation for why this is critical
        final key = ValueKey(rowId);

        final renderContext = RenderContext(
          rowData: rowData,
          onOperation: widget.onOperation,
        );

        return KeyedSubtree(
          key: key,
          child: _interpreter.build(itemExpr, renderContext),
        );
      },
    );
  }

  /// Build single item (non-list rendering).
  Widget _buildSingleItem(RenderExpr expr) {
    if (_rowCache.isEmpty) {
      return _buildEmpty();
    }

    // For single items, use first row in cache
    final rowId = _rowOrder.first;
    final rowData = _rowCache[rowId]!;

    final renderContext = RenderContext(
      rowData: rowData,
      onOperation: widget.onOperation,
    );

    return _interpreter.build(expr, renderContext);
  }

  /// Build empty state.
  Widget _buildEmpty() {
    return const Center(
      child: Text('No data'),
    );
  }

  /// Build error state.
  Widget _buildError(Object error) {
    return Center(
      child: Column(
        mainAxisAlignment: MainAxisAlignment.center,
        children: [
          const Icon(Icons.error, color: Colors.red, size: 48),
          const SizedBox(height: 16),
          Text(
            'Error: ${error.toString()}',
            style: const TextStyle(color: Colors.red),
          ),
        ],
      ),
    );
  }

  @override
  void dispose() {
    // TODO Phase 4.1: Dispose Rust stream subscription
    super.dispose();
  }
}

/// Widget for nested queries (queries within queries).
///
/// This creates its own ReactiveQueryWidget with a separate CDC stream.
/// Lifecycle:
/// - Lazy loading: Query starts when widget builds (scrolled into view)
/// - Auto-disposal: Stream disposed when widget removed from tree
///
/// Example:
/// A block contains a live table showing related tasks:
/// ```prql
/// render list(block(
///   text(content),
///   nested_query("SELECT * FROM tasks WHERE block_id = blocks.id")
/// ))
/// ```
class NestedQueryWidget extends StatelessWidget {
  /// SQL for nested query.
  final String sql;

  /// Parameters for nested query (can reference parent row columns).
  final Map<String, dynamic> params;

  /// Render spec for nested query results.
  final RenderSpec renderSpec;

  /// Callback for operations.
  final Future<void> Function(String operationName, Map<String, dynamic> params)? onOperation;

  const NestedQueryWidget({
    super.key,
    required this.sql,
    required this.params,
    required this.renderSpec,
    this.onOperation,
  });

  @override
  Widget build(BuildContext context) {
    // Create nested ReactiveQueryWidget with own stream
    return ReactiveQueryWidget(
      sql: sql,
      params: params,
      renderSpec: renderSpec,
      onOperation: onOperation,
    );
  }
}
