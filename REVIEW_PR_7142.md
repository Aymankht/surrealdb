# Code Review: PR #7142 — Avoid Eager Computed Field Evaluation

**PR:** surrealdb/surrealdb#7142
**Issue:** #7094 — Eager evaluation of COMPUTED fields causes unintended side-effects, traversal failures, and infinite recursion
**Reviewer:** Claude (Senior Maintainer Review)
**Verdict:** Request Changes (with actionable feedback)

---

## High-Level Summary

This PR introduces **selective computed field evaluation** for `SELECT` statements. Instead of eagerly computing all `COMPUTED` fields whenever a record is loaded, the system now:

1. Extracts the set of field names actually needed by the query (projections, WHERE, ORDER BY, GROUP BY, SPLIT).
2. Builds a dependency graph for computed fields using pre-cached `ComputedDeps` metadata (or falls back to runtime extraction).
3. Computes the transitive closure of required computed fields.
4. Skips evaluation of computed fields not in the required set.

The architecture is sound: a new `computed_deps.rs` module provides dependency extraction (visitor-based), topological sorting (Kahn's algorithm), and transitive closure resolution. The `pluck_select` path threads the needed-field set through to `computed_fields()`, which gates evaluation per-field.

**Key files changed:**
- `surrealdb/core/src/expr/computed_deps.rs` — New module (dependency extraction, topo sort, transitive closure)
- `surrealdb/core/src/doc/compute.rs` — Modified to accept optional `needed_roots` parameter
- `surrealdb/core/src/doc/pluck.rs` — `pluck_select()` now calls `extract_needed_fields()` and passes result to `computed_fields()`
- `surrealdb/core/src/doc/check.rs` — Updated call sites to pass `None` (full compute)
- `surrealdb/core/src/doc/lives.rs` — Updated call site to pass `None`
- `surrealdb/core/src/exec/planner/select.rs` — New `extract_needed_fields()` function
- `language-tests/tests/reproductions/7094_computed_fields_not_selected.surql` — Regression test

---

## Line-by-Line Breakdown

### 1. `computed_deps.rs` — Dependency Extraction Module

**Positive:**
- Clean visitor-based design using the existing `Visit`/`Visitor` infrastructure.
- Correct handling of opaque constructs (subqueries, params, closures, graph traversals) by marking `is_complete = false`.
- Topological sort using Kahn's algorithm is correct and handles cycles gracefully as a safety net.
- `resolve_required_computed_fields()` correctly implements transitive closure with an early `None` bail on incomplete dependencies.
- Comprehensive unit test coverage (8 extraction tests, 4 topo sort tests, 3 transitive closure tests).

**Issues:**

**(a) `topological_sort_computed_fields` cycle detection is O(n^2) — Line 186**
```rust
if !result.contains(&idx) {
    result.push(idx);
}
```
`result.contains()` is O(n) on a Vec, making the cycle-fallback O(n^2). For a safety net this is acceptable since cycles should be caught at DEFINE time, but worth noting. A `HashSet` could be used if field counts are ever large.

**(b) Sorted dep fields add determinism but cost an allocation — Line 43-46**
```rust
let mut fields: Vec<String> = extractor.deps.into_iter().collect();
fields.sort();
```
This sort is good for deterministic test output and caching, but adds a small allocation cost per extraction. This is fine since extraction happens once at DEFINE time and is cached.

### 2. `compute.rs` — Core Selective Evaluation Logic

**Positive:**
- Clean separation: `computed_fields()` handles the dep resolution; `computed_fields_inner()` handles actual evaluation.
- `fields_computed = true` is only set for full evaluation (`required.is_none()`), correctly allowing a subsequent full-compute to run if needed.
- Early exit when no required computed fields exist (line 104) avoids unnecessary work.

**Issues:**

**(a) CRITICAL: Duplicate dep_map construction — Lines 61-78**
The dependency map is built from scratch on every `computed_fields()` call with `needed_roots`. This same logic exists in `pipeline.rs` (lines 520-543). This should be cached or at minimum shared.

```rust
// This block runs on EVERY record for selective evaluation:
let mut dep_map: HashMap<String, ComputedDeps> = HashMap::new();
for fd in table_fields.iter() {
    if fd.computed.is_none() { continue; }
    let field_name = fd.name.to_raw_string();
    let deps = if let Some(cd) = &fd.computed_deps { ... } else { ... };
    dep_map.insert(field_name, deps);
}
```

For a table scan of 100K records, this builds the same HashMap 100K times. The `FieldDefinition` already caches `computed_deps`, but the `HashMap` construction is repeated. **Recommendation:** Pre-compute and cache the `dep_map` in the `DocContext` or pass it once from the caller.

**(b) `to_raw_string()` called twice per field — Lines 66 and 147**
In `computed_fields()` and `computed_fields_inner()`, `fd.name.to_raw_string()` is called separately. This is a String allocation per computed field per record. Minor but adds up.

**(c) Potential correctness issue with selective + full interaction**
If a partial compute runs first (from `pluck_select`), `fields_computed` stays `false`. If the same doc later goes through a full-compute path (e.g., LIVE query in `lives.rs`), the full compute will re-evaluate ALL fields including ones already computed. This means some computed fields could execute **twice** — once selectively and once fully. This is semantically safe (idempotent overwrite via `put`) but wastes CPU.

### 3. `pluck.rs` — Integration into pluck_select

**Positive:**
- `pluck_select()` correctly calls `extract_needed_fields()` with all relevant clause information (fields, omit, cond, order, group, split).
- `pluck_generic()` correctly passes `None` for non-SELECT paths (CREATE, UPDATE, etc.) where all computed fields must be evaluated.

**Issues:**

**(a) CRITICAL: `FETCH` clause not accounted for — Line 212-219**
`extract_needed_fields()` does not receive the `FETCH` clause. FETCH traverses into linked records and may reference fields that need to be computed. For example:

```sql
SELECT name FROM person FETCH friends
```

If `friends` is a record link and the linked record has computed fields, those fields won't be eagerly computed because `friends` is in the needed set but its *inner* fields aren't analyzed. However, the bigger concern is:

```sql
SELECT id FROM person FETCH computed_link
```

If `computed_link` is a computed field producing a record ID, and it's only in the FETCH clause (not in the projection), it won't be computed because `extract_needed_fields` doesn't walk FETCH.

**Recommendation:** Add `stmt.fetch` to the `extract_needed_fields()` call and walk FETCH idioms in the extractor.

**(b) CRITICAL: WHERE clause forces full computation, defeating the optimization**
The call chain is:
```
select() → check_select_where_condition() → computed_fields(..., None) [FULL]
         → pluck_select() → computed_fields(..., needed_roots) [SELECTIVE, but no-op]
```

`check_select_where_condition` (check.rs:271-274) always passes `None`, triggering full computation and setting `fields_computed = true`. When `pluck_select` subsequently calls `computed_fields` with `needed_roots`, the inner function bails immediately because `fields_computed` is already `true`.

**This means the optimization only works for SELECT queries WITHOUT a WHERE clause.** Any `SELECT a FROM table WHERE ...` will still eagerly compute all fields.

**Recommendation:** `check_select_where_condition` should also extract needed fields from the WHERE clause and only compute those. Or better: merge the WHERE-condition field needs with the projection field needs and compute once.

### 4. `check.rs` — WHERE Condition Paths

All four call sites correctly pass `None` for backward compatibility. The non-SELECT WHERE path (`check_where_condition`) used by UPDATE/DELETE/etc. correctly does full computation since those statements need all fields for side-effects.

No issues here beyond the missed optimization opportunity described above.

### 5. `lives.rs` — Live Query Path

Line 191 correctly passes `None` for live queries, which need full materialization. No issues.

### 6. `select.rs` (planner) — `extract_needed_fields()`

**Positive:**
- Covers all relevant clauses: projections, OMIT, WHERE, ORDER BY, GROUP BY, SPLIT.
- Correctly returns `None` (compute all) for SELECT * and opaque expressions.
- Visitor design is clean and matches the `computed_deps.rs` pattern.

**Issues:**

**(a) Missing FETCH clause** (same as pluck.rs issue above)

**(b) OMIT fields probably should NOT be in the needed set — Lines 1013-1016**
```rust
// Walk OMIT fields (they may reference computed fields that need evaluation)
for expr in omit {
    let _ = extractor.visit_expr(expr);
}
```

OMIT specifies fields to *exclude* from the output. If a user writes `SELECT * OMIT expensive_field FROM table`, the intent is to avoid computing `expensive_field`. But this code adds OMIT'd field names to the needed set, which means they'll be computed anyway.

However, `extract_needed_fields` returns `None` for `SELECT *`, so this code only applies when specific fields are projected alongside OMIT (unusual). The comment says "they may reference computed fields that need evaluation" but this reasoning seems inverted — OMIT fields are precisely the ones we want to *skip*.

**Recommendation:** Remove OMIT fields from the needed set, or only include them if they appear in a WHERE/ORDER/GROUP context.

**(c) `SELECT VALUE expr` does not trigger wildcard bail — Lines 952-954**
This is correct since `SELECT VALUE` always evaluates a single expression, but if `expr` is `*` this could be an issue. Verify that `SELECT VALUE *` is handled.

### 7. Language Test — `7094_computed_fields_not_selected.surql`

**Positive:**
- Covers all three scenarios from the issue: THROW in unselected field, graph traversal with unrelated computed field, self-referential computed field.
- Uses `SCHEMAFULL` tables which is the realistic use case.

**Issues:**

**(a) All test results use `error = false` — no value assertions**
The test only checks that queries don't error. It does not verify that the correct values are returned. For example, Case 1 should assert that `SELECT should_error FROM ONLY test_table_7094:0` returns `{ should_error: true }`.

**(b) Missing test cases:**
- **SELECT * with computed fields** — should still compute all fields.
- **SELECT with WHERE referencing a computed field** — should compute that field for filtering.
- **SELECT with ORDER BY on a computed field** — should compute that field for sorting.
- **Transitive dependency test** — computed field A depends on computed field B; selecting A should also compute B.
- **Computed field with `is_complete = false`** (subquery/param) — should fall back to full computation.
- **UPDATE/CREATE with RETURN clause** — should still compute all fields.

---

## Constructive Feedback & Blockers

### Blockers (Must Fix)

1. **WHERE clause defeats the optimization.** The `check_select_where_condition` path eagerly computes all fields with `None`, setting `fields_computed = true` before `pluck_select` can apply selective computation. Since most real-world queries have WHERE clauses, this severely limits the PR's impact. The fix is to either (a) thread needed fields into the WHERE check path, or (b) avoid setting `fields_computed` in that path and let `pluck_select` handle it.

2. **FETCH clause not in `extract_needed_fields`.** Computed fields referenced only in FETCH won't be evaluated, leading to missing data in FETCH results.

3. **Per-record dep_map construction.** Building a `HashMap` from `FieldDefinition` on every single record is wasteful. For large scans, this is a meaningful regression in the no-WHERE case where the optimization does apply. Cache this at the table/scan level.

### Should Fix

4. **OMIT fields should not be added to the needed set.** This partially defeats the purpose of OMIT for computed fields.

5. **Test assertions should verify values, not just absence of errors.** `error = false` doesn't catch regressions where fields return wrong values.

6. **More test coverage needed** for WHERE, ORDER BY, GROUP BY, transitive deps, and opaque fallback scenarios.

### Nice to Have

7. **Deduplicate dep_map construction** between `compute.rs` and `pipeline.rs`. Consider a shared helper.

8. **`to_raw_string()` allocations** could be reduced by comparing against an `&str` rather than constructing a String per field per record.

---

## Performance Analysis

| Scenario | Before PR | After PR | Notes |
|----------|-----------|----------|-------|
| `SELECT specific_field FROM table` (no WHERE) | Computes ALL fields | Computes only needed fields | **Improved** |
| `SELECT specific_field FROM table WHERE ...` | Computes ALL fields | Computes ALL fields (twice: WHERE + pluck) | **No change** (blocked by WHERE path) |
| `SELECT * FROM table` | Computes ALL fields | Computes ALL fields | No change (correct) |
| Any record with no computed fields | Fast path (no-op) | Fast path + HashMap construction | **Minor regression** from dep_map build |

The PR adds ~200 bytes of allocation per record (HashMap + HashSet) even when the optimization cannot apply. For the happy path (no WHERE, specific fields), the savings are significant.

---

## Verdict: Request Changes

The architectural approach is excellent — dependency extraction, topological sorting, and transitive closure are all well-implemented. The `computed_deps.rs` module is production-quality with good test coverage.

However, **three blockers** prevent approval:

1. **The WHERE clause path defeats the optimization** for the majority of real-world queries.
2. **FETCH is not accounted for**, which can cause data loss.
3. **Per-record HashMap construction** introduces a performance cost that should be amortized.

Once these are addressed, this PR will be a high-impact improvement for SurrealDB's query execution efficiency.
