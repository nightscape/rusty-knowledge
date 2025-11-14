import 'package:flutter/material.dart';
import '../src/rust/third_party/query_render/types.dart';

/// Context passed to widget builders during rendering.
/// Contains row data and configuration needed to build widgets.
class RenderContext {
  /// Current row data being rendered (from SQL query results).
  final Map<String, dynamic> rowData;

  /// Callback for executing operations (indent, outdent, etc.).
  final Future<void> Function(String operationName, Map<String, dynamic> params)? onOperation;

  /// Configuration for nested queries (if applicable).
  final Map<String, dynamic>? nestedQueryConfig;

  const RenderContext({
    required this.rowData,
    this.onOperation,
    this.nestedQueryConfig,
  });

  /// Get a column value from row data, returns null if not found.
  dynamic getColumn(String name) => rowData[name];

  /// Get a column value with type casting, throws if type mismatch.
  T getTypedColumn<T>(String name) {
    final value = rowData[name];
    if (value is! T) {
      throw ArgumentError('Column $name is not of type $T (got ${value.runtimeType})');
    }
    return value;
  }
}

/// Interprets generic RenderExpr AST and builds Flutter widgets.
///
/// This interpreter maps function calls to Flutter widgets:
/// - `list(...)` → ListView.builder
/// - `block(...)` → Column with indentation
/// - `editable_text(...)` → TextField
/// - `row(...)` → Row
/// - Custom functions can be added via extensibility
class RenderInterpreter {
  /// Build a widget from a RenderExpr using the provided context.
  Widget build(RenderExpr expr, RenderContext context) {
    return expr.when(
      functionCall: (name, args) => _buildFunctionCall(name, args, context),
      columnRef: (name) => _buildColumnRef(name, context),
      literal: (value) => _buildLiteral(value),
      binaryOp: (op, left, right) => _buildBinaryOp(op, left, right, context),
      array: (items) => _buildArray(items, context),
      object: (fields) => _buildObject(fields, context),
    );
  }

  /// Build widget from function call (main widget mapping logic).
  Widget _buildFunctionCall(String name, List<Arg> args, RenderContext context) {
    final namedArgs = <String, RenderExpr>{};
    final positionalArgs = <RenderExpr>[];

    for (final arg in args) {
      if (arg.name != null) {
        namedArgs[arg.name!] = arg.value;
      } else {
        positionalArgs.add(arg.value);
      }
    }

    switch (name) {
      case 'list':
        return _buildList(namedArgs, context);
      case 'block':
        return _buildBlock(namedArgs, positionalArgs, context);
      case 'row':
        return _buildRow(namedArgs, positionalArgs, context);
      case 'editable_text':
        return _buildEditableText(namedArgs, context);
      case 'text':
        return _buildText(namedArgs, positionalArgs, context);
      case 'drop_zone':
        return _buildDropZone(namedArgs, context);
      case 'collapse_button':
        return _buildCollapseButton(namedArgs, context);
      case 'block_operations':
        return _buildBlockOperations(namedArgs, context);
      case 'flexible':
        return _buildFlexible(namedArgs, positionalArgs, context);
      default:
        return _buildUnknownFunction(name, args);
    }
  }

  /// Build Flexible wrapper from flexible() function.
  /// Used to provide flex constraints to children in Row/Column.
  Widget _buildFlexible(
    Map<String, RenderExpr> namedArgs,
    List<RenderExpr> positionalArgs,
    RenderContext context,
  ) {
    if (positionalArgs.isEmpty) {
      throw ArgumentError('flexible() requires a child argument');
    }

    final child = build(positionalArgs[0], context);

    // Optional flex factor (default 1)
    final flexExpr = namedArgs['flex'];
    final flex = flexExpr != null ? _evaluateToInt(flexExpr, context) : 1;

    return Flexible(
      flex: flex,
      child: child,
    );
  }

  /// Build ListView from list() function.
  Widget _buildList(Map<String, RenderExpr> args, RenderContext renderContext) {
    final itemExpr = args['item'];
    if (itemExpr == null) {
      throw ArgumentError('list() requires "item" argument');
    }

    // For now, build a single item. In Phase 4.1, this will be replaced
    // with StreamBuilder that listens to CDC events and builds multiple items.
    return ListView.builder(
      itemCount: 1,
      itemBuilder: (buildContext, index) => build(itemExpr, renderContext),
    );
  }

  /// Build Column with indentation from block() function.
  Widget _buildBlock(
    Map<String, RenderExpr> namedArgs,
    List<RenderExpr> positionalArgs,
    RenderContext context,
  ) {
    final children = <Widget>[];

    // Add all positional arguments as children
    for (final arg in positionalArgs) {
      children.add(build(arg, context));
    }

    // Get depth for indentation (optional)
    final depthExpr = namedArgs['depth'];
    final depth = depthExpr != null
        ? _evaluateToInt(depthExpr, context)
        : 0;

    final indentPixels = depth * 24.0;

    return Padding(
      padding: EdgeInsets.only(left: indentPixels),
      child: Column(
        crossAxisAlignment: CrossAxisAlignment.start,
        mainAxisSize: MainAxisSize.min,
        children: children,
      ),
    );
  }

  /// Build Row from row() function.
  Widget _buildRow(
    Map<String, RenderExpr> namedArgs,
    List<RenderExpr> positionalArgs,
    RenderContext context,
  ) {
    final children = positionalArgs.map((arg) => build(arg, context)).toList();

    return Row(
      mainAxisSize: MainAxisSize.min,
      children: children,
    );
  }

  /// Build TextField from editable_text() function.
  Widget _buildEditableText(Map<String, RenderExpr> args, RenderContext context) {
    final contentExpr = args['content'];
    final content = contentExpr != null
        ? _evaluateToString(contentExpr, context)
        : '';

    // TODO Phase 4.3: Wire up to operation execution for content updates
    // Note: TextField needs bounded width constraints. When used in Row,
    // wrap it in Flexible/Expanded in the PRQL query itself, e.g.:
    // row(collapse_button(), flexible(editable_text(content)))
    return TextField(
      controller: TextEditingController(text: content),
      decoration: const InputDecoration(
        border: InputBorder.none,
        isDense: true,
        contentPadding: EdgeInsets.zero,
      ),
      style: const TextStyle(fontSize: 16),
    );
  }

  /// Build Text widget from text() function.
  Widget _buildText(
    Map<String, RenderExpr> namedArgs,
    List<RenderExpr> positionalArgs,
    RenderContext context,
  ) {
    String text;
    if (positionalArgs.isNotEmpty) {
      text = _evaluateToString(positionalArgs[0], context);
    } else if (namedArgs['value'] != null) {
      text = _evaluateToString(namedArgs['value']!, context);
    } else {
      text = '';
    }

    return Text(text);
  }

  /// Build drag target (drop zone) from drop_zone() function.
  Widget _buildDropZone(Map<String, RenderExpr> args, RenderContext context) {
    final positionExpr = args['position'];
    final position = positionExpr != null
        ? _evaluateToString(positionExpr, context)
        : 'before';

    // TODO Phase 4.2: Implement full drag-drop with DragTarget
    // TODO: Parse invalid_targets from args
    // TODO: Wire up on_drop callback to operation execution

    return Container(
      height: 4,
      color: Colors.transparent,
      child: Center(
        child: Container(
          height: 2,
          color: Colors.blue.withValues(alpha: 0.0),
        ),
      ),
    );
  }

  /// Build collapse/expand button from collapse_button() function.
  Widget _buildCollapseButton(Map<String, RenderExpr> args, RenderContext context) {
    final isCollapsedExpr = args['is_collapsed'];
    final isCollapsed = isCollapsedExpr != null
        ? _evaluateToBool(isCollapsedExpr, context)
        : false;

    // TODO Phase 4.2: Wire up to toggle_collapse operation
    return IconButton(
      icon: Icon(isCollapsed ? Icons.chevron_right : Icons.expand_more),
      iconSize: 20,
      padding: EdgeInsets.zero,
      constraints: const BoxConstraints(),
      onPressed: () {
        // TODO: Execute toggle_collapse operation
      },
    );
  }

  /// Build block operations menu from block_operations() function.
  Widget _buildBlockOperations(Map<String, RenderExpr> args, RenderContext context) {
    // TODO Phase 4.2: Build operation buttons/menu
    // TODO: Parse operations list from args
    // TODO: Wire up to operation execution

    return Row(
      mainAxisSize: MainAxisSize.min,
      children: [
        IconButton(
          icon: const Icon(Icons.more_horiz),
          iconSize: 20,
          padding: EdgeInsets.zero,
          constraints: const BoxConstraints(),
          onPressed: () {
            // TODO: Show operations menu
          },
        ),
      ],
    );
  }

  /// Build placeholder for unknown functions.
  Widget _buildUnknownFunction(String name, List<Arg> args) {
    return Container(
      padding: const EdgeInsets.all(8),
      color: Colors.red.withValues(alpha: 0.1),
      child: Text(
        'Unknown function: $name',
        style: const TextStyle(color: Colors.red),
      ),
    );
  }

  /// Build widget from column reference (e.g., `block_id`, `content`).
  Widget _buildColumnRef(String name, RenderContext context) {
    final value = context.getColumn(name);
    return Text(value?.toString() ?? '');
  }

  /// Build widget from literal value.
  Widget _buildLiteral(Value value) {
    return value.when(
      null_: () => const Text('null'),
      bool: (b) => Text(b.toString()),
      number: (n) => n.when(
        int: (i) => Text(i.toString()),
        float: (f) => Text(f.toString()),
      ),
      string: (s) => Text(s),
      array: (items) => Text('[${items.length} items]'),
      object: (fields) => Text('{${fields.length} fields}'),
    );
  }

  /// Build widget from binary operation (e.g., `depth * 24`, `completed and visible`).
  Widget _buildBinaryOp(
    BinaryOperator op,
    RenderExpr left,
    RenderExpr right,
    RenderContext context,
  ) {
    // Evaluate binary operation to a value, then display
    final result = _evaluateBinaryOp(op, left, right, context);
    return Text(result.toString());
  }

  /// Build widget from array literal.
  Widget _buildArray(List<RenderExpr> items, RenderContext context) {
    final children = items.map((item) => build(item, context)).toList();
    return Column(
      crossAxisAlignment: CrossAxisAlignment.start,
      mainAxisSize: MainAxisSize.min,
      children: children,
    );
  }

  /// Build widget from object literal.
  Widget _buildObject(Map<String, RenderExpr> fields, RenderContext context) {
    // Objects are typically not rendered directly, but used as arguments
    return Text('{${fields.keys.join(', ')}}');
  }

  // --- Expression Evaluation Helpers ---

  /// Evaluate expression to integer value.
  int _evaluateToInt(RenderExpr expr, RenderContext context) {
    return expr.when(
      literal: (value) => value.when(
        number: (n) => n.when(
          int: (i) => i,
          float: (f) => f.toInt(),
        ),
        null_: () => 0,
        bool: (_) => throw ArgumentError('Cannot convert bool to int'),
        string: (_) => throw ArgumentError('Cannot convert string to int'),
        array: (_) => throw ArgumentError('Cannot convert array to int'),
        object: (_) => throw ArgumentError('Cannot convert object to int'),
      ),
      columnRef: (name) {
        final value = context.getColumn(name);
        if (value is int) return value;
        if (value is double) return value.toInt();
        throw ArgumentError('Column $name is not numeric');
      },
      binaryOp: (op, left, right) {
        final result = _evaluateBinaryOp(op, left, right, context);
        if (result is int) return result;
        if (result is double) return result.toInt();
        throw ArgumentError('Binary operation did not produce numeric result');
      },
      functionCall: (_, __) => throw ArgumentError('Cannot evaluate function call to int'),
      array: (_) => throw ArgumentError('Cannot evaluate array to int'),
      object: (_) => throw ArgumentError('Cannot evaluate object to int'),
    );
  }

  /// Evaluate expression to string value.
  String _evaluateToString(RenderExpr expr, RenderContext context) {
    return expr.when(
      literal: (value) => value.when(
        string: (s) => s,
        number: (n) => n.when(
          int: (i) => i.toString(),
          float: (f) => f.toString(),
        ),
        bool: (b) => b.toString(),
        null_: () => '',
        array: (_) => throw ArgumentError('Cannot convert array to string'),
        object: (_) => throw ArgumentError('Cannot convert object to string'),
      ),
      columnRef: (name) => context.getColumn(name)?.toString() ?? '',
      binaryOp: (op, left, right) => _evaluateBinaryOp(op, left, right, context).toString(),
      functionCall: (_, __) => throw ArgumentError('Cannot evaluate function call to string'),
      array: (_) => throw ArgumentError('Cannot evaluate array to string'),
      object: (_) => throw ArgumentError('Cannot evaluate object to string'),
    );
  }

  /// Evaluate expression to boolean value.
  bool _evaluateToBool(RenderExpr expr, RenderContext context) {
    return expr.when(
      literal: (value) => value.when(
        bool: (b) => b,
        null_: () => false,
        number: (n) => n.when(
          int: (i) => i != 0,
          float: (f) => f != 0.0,
        ),
        string: (s) => s.isNotEmpty,
        array: (items) => items.isNotEmpty,
        object: (fields) => fields.isNotEmpty,
      ),
      columnRef: (name) {
        final value = context.getColumn(name);
        if (value is bool) return value;
        if (value == null) return false;
        throw ArgumentError('Column $name is not boolean');
      },
      binaryOp: (op, left, right) {
        final result = _evaluateBinaryOp(op, left, right, context);
        if (result is bool) return result;
        throw ArgumentError('Binary operation did not produce boolean result');
      },
      functionCall: (_, __) => throw ArgumentError('Cannot evaluate function call to bool'),
      array: (_) => throw ArgumentError('Cannot evaluate array to bool'),
      object: (_) => throw ArgumentError('Cannot evaluate object to bool'),
    );
  }

  /// Evaluate binary operation to a value.
  dynamic _evaluateBinaryOp(
    BinaryOperator op,
    RenderExpr left,
    RenderExpr right,
    RenderContext context,
  ) {
    switch (op) {
      // Comparison operators
      case BinaryOperator.eq:
        return _evaluateGeneric(left, context) == _evaluateGeneric(right, context);
      case BinaryOperator.neq:
        return _evaluateGeneric(left, context) != _evaluateGeneric(right, context);
      case BinaryOperator.gt:
        return _compareNumeric(left, right, context, (a, b) => a > b);
      case BinaryOperator.lt:
        return _compareNumeric(left, right, context, (a, b) => a < b);
      case BinaryOperator.gte:
        return _compareNumeric(left, right, context, (a, b) => a >= b);
      case BinaryOperator.lte:
        return _compareNumeric(left, right, context, (a, b) => a <= b);

      // Arithmetic operators
      case BinaryOperator.add:
        return _evaluateToNum(left, context) + _evaluateToNum(right, context);
      case BinaryOperator.sub:
        return _evaluateToNum(left, context) - _evaluateToNum(right, context);
      case BinaryOperator.mul:
        return _evaluateToNum(left, context) * _evaluateToNum(right, context);
      case BinaryOperator.div:
        return _evaluateToNum(left, context) / _evaluateToNum(right, context);

      // Logical operators
      case BinaryOperator.and:
        return _evaluateToBool(left, context) && _evaluateToBool(right, context);
      case BinaryOperator.or:
        return _evaluateToBool(left, context) || _evaluateToBool(right, context);
    }
  }

  /// Evaluate expression to num (int or double).
  num _evaluateToNum(RenderExpr expr, RenderContext context) {
    return expr.when(
      literal: (value) => value.when(
        number: (n) => n.when(
          int: (i) => i,
          float: (f) => f,
        ),
        null_: () => 0,
        bool: (_) => throw ArgumentError('Cannot convert bool to num'),
        string: (_) => throw ArgumentError('Cannot convert string to num'),
        array: (_) => throw ArgumentError('Cannot convert array to num'),
        object: (_) => throw ArgumentError('Cannot convert object to num'),
      ),
      columnRef: (name) {
        final value = context.getColumn(name);
        if (value is num) return value;
        throw ArgumentError('Column $name is not numeric');
      },
      binaryOp: (op, left, right) {
        final result = _evaluateBinaryOp(op, left, right, context);
        if (result is num) return result;
        throw ArgumentError('Binary operation did not produce numeric result');
      },
      functionCall: (_, __) => throw ArgumentError('Cannot evaluate function call to num'),
      array: (_) => throw ArgumentError('Cannot evaluate array to num'),
      object: (_) => throw ArgumentError('Cannot evaluate object to num'),
    );
  }

  /// Evaluate expression to generic dynamic value.
  dynamic _evaluateGeneric(RenderExpr expr, RenderContext context) {
    return expr.when(
      literal: (value) => _valueToNative(value),
      columnRef: (name) => context.getColumn(name),
      binaryOp: (op, left, right) => _evaluateBinaryOp(op, left, right, context),
      functionCall: (_, __) => throw ArgumentError('Cannot evaluate function call generically'),
      array: (items) => items.map((item) => _evaluateGeneric(item, context)).toList(),
      object: (fields) => fields.map((key, value) => MapEntry(key, _evaluateGeneric(value, context))),
    );
  }

  /// Convert Value to native Dart type.
  dynamic _valueToNative(Value value) {
    return value.when(
      null_: () => null,
      bool: (b) => b,
      number: (n) => n.when(
        int: (i) => i,
        float: (f) => f,
      ),
      string: (s) => s,
      array: (items) => items.map(_valueToNative).toList(),
      object: (fields) => fields.map((key, value) => MapEntry(key, _valueToNative(value))),
    );
  }

  /// Compare two numeric expressions.
  bool _compareNumeric(
    RenderExpr left,
    RenderExpr right,
    RenderContext context,
    bool Function(num, num) compare,
  ) {
    final leftVal = _evaluateToNum(left, context);
    final rightVal = _evaluateToNum(right, context);
    return compare(leftVal, rightVal);
  }
}
