# PRQL Syntax Analysis: Why Original Nested Syntax Doesn't Work

## Summary

The original nested function call syntax **cannot work** with PRQL's lineage tracking due to fundamental scoping limitations in nested expressions.

## Original Desired Syntax

```prql
render (list hierarchical_sort:[parent_id, sort_key] item_template:(row (checkbox checked:completed) (text content:id) ...))
```

## The Core Problem: Scoping in Nested Function Calls

When you nest function calls in PRQL, **the inner function cannot access columns or values from the outer scope**. This includes:

1. **Relation columns** (`parent_id`, `sort_key`)
2. **Qualified columns** (`this.parent_id`, `blocks.parent_id`)
3. **Derived columns** from previous `derive` statements
4. **The `this` reference** to the current relation

### Test Results

| Syntax Attempted | Column Reference | Result |
|-----------------|------------------|---------|
| `hierarchical_sort:[parent_id, sort_key]` | Unqualified | ❌ `Unknown name 'parent_id'` |
| `hierarchical_sort:[this.parent_id, this.sort_key]` | `this` qualified | ❌ `Unknown name 'this.parent_id'` |
| `hierarchical_sort:[blocks.parent_id, blocks.sort_key]` | Table qualified | ❌ `Unknown name 'blocks.parent_id'` |
| `sort_cols:sort_cols_val` with prior derive | Derived column | ❌ `Unknown name 'sort_cols_val'` |

**Conclusion**: Nested function calls create an isolated scope where NO external columns are accessible.

## Why Sequential Derives Work

```prql
# Stage 1: Create UI elements (these CAN access relation columns)
derive {
    checkbox_el = (ui_checkbox checked:this.completed),
    id_el = (ui_text text_content:this.id),
    ...
}

# Stage 2: Compose (these CAN access Stage 1 derived columns AND relation columns)
derive {
    row_el = (ui_row items:[checkbox_el, id_el, ...]),
    list_output = (ui_list rel:this hierarchical_sort:[parent_id, sort_key] item_template:row_el),
    ...
}
```

**Why it works**:
1. Each `derive` statement executes at the pipeline level (not nested)
2. At pipeline level, relation columns (`parent_id`, `sort_key`) ARE accessible
3. Each `derive` can reference columns from previous pipeline stages
4. The `this` reference works at pipeline level

## The Scoping Rule

```
Pipeline Level (derive, select, filter, etc.)
  ✅ Can access relation columns
  ✅ Can access derived columns
  ✅ Can use `this` reference
  ✅ Can call functions with parameters

  Nested Function Calls (inside expressions)
    ❌ Cannot access relation columns
    ❌ Cannot access derived columns
    ❌ Cannot use `this` reference
    ✅ Can only use function parameters
    ✅ Can use literals
```

## Attempted Workarounds (All Failed)

### 1. Using `this` Reference in Nested Call
```prql
ui_render (ui_list this hierarchical_sort:[parent_id, sort_key] ...)
```
❌ **Failed**: `Unknown name 'parent_id'` - columns not accessible

### 2. Fully Qualifying Columns
```prql
ui_render (ui_list this hierarchical_sort:[blocks.parent_id, blocks.sort_key] ...)
```
❌ **Failed**: `Unknown name 'blocks.parent_id'` - even qualified names don't work

### 3. Passing Through Derived Column
```prql
derive {
    ...,
    sort_cols_val = [parent_id, sort_key]  # Works at this level
}
ui_render (ui_list this sort_cols:sort_cols_val ...)
```
❌ **Failed**: `Unknown name 'sort_cols_val'` - derived column not accessible in nested call

### 4. Using Pipeline Transform Instead of Select
```prql
derive { ..., list_output = (ui_list ...) }
ui_render rel:list_output
```
❌ **Failed**: `Unknown name 'list_output'` - can't reference derived column as pipeline transform

## Why PRQL Has This Limitation

This is likely by design for several reasons:

1. **SQL Translation**: PRQL compiles to SQL. Deeply nested expressions would create complex subqueries that are hard to optimize

2. **Type Safety**: Limiting scope makes type checking and column resolution more predictable

3. **Performance**: Keeping scope shallow allows the compiler to better optimize column references

4. **Clarity**: Sequential pipelines are more readable and maintainable than deeply nested expressions

## The Working Pattern

**✅ Multiple Sequential Derives**:
```prql
from table
derive { stage1_columns = ... }   # Access relation columns
derive { stage2_columns = ... }   # Access stage1 AND relation columns
derive { stage3_columns = ... }   # Access stage1, stage2 AND relation columns
select final_column                # Access all previous columns
```

**❌ Nested Function Calls**:
```prql
from table
transform (nested (deeply (fn (column))))  # ❌ columns not accessible
```

## Implications for UI Rendering

For UI rendering frameworks that use nested syntax, you have two options:

### Option 1: Flatten to Sequential Stages (Current Approach)
```prql
derive { ui_elements = ... }
derive { composed_elements = ... }
derive { final_output = ... }
select final_output
```
**Pros**: Works with lineage, clear data flow
**Cons**: More verbose, not as declarative

### Option 2: Compile Nested Syntax to Sequential (Preprocessor)
Create a preprocessor that transforms:
```prql
render (list (row (checkbox ...) (text ...) ...))
```

Into:
```prql
derive { checkbox = ..., text = ..., ... }
derive { row = ..., list = ..., render = ... }
```

**Pros**: Clean syntax, generates working PRQL
**Cons**: Requires additional tooling

## Key Takeaway

**The original nested syntax is fundamentally incompatible with PRQL's scoping rules.** To use custom UI functions with lineage tracking, you MUST use sequential `derive` stages at the pipeline level rather than nested function calls.

This is not a bug or limitation we can work around - it's a fundamental design constraint of PRQL's compilation model.
