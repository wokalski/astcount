---
name: astcount-refactor-loop
description: Autonomously reduce code complexity through repeated astcount-guided, behavior-preserving refactors and relevant tests until credible structural gains are exhausted. Use for a sustained refactor loop; use an interactive variant when the user wants to choose tradeoffs, or a verified variant when one exact test command must gate every candidate.
---

# Run an astcount refactor loop

Keep finding and applying readable, behavior-preserving simplifications that
reduce Tree-sitter named-node count. Do not stop after the first successful
decrease or ask whether to continue merely because candidates succeeded.

## Establish the measurement

Infer the scope from the request, defaulting to the repository. Resolve one
astcount command, run it with `--version`, and reuse that exact invocation:

1. When `command -v nix` succeeds, use
   `nix run github:wokalski/astcount/v0.3.0 --`.
2. Otherwise, use `bunx astcount@0.3.0` when Bun is available.
3. Otherwise, use `npx --yes astcount@0.3.0` when npx is available.
4. Stop if none is available.

Choose a fixed filter policy before the baseline. Start with
`--exclude-kind anonymous` for named-node complexity, then inspect the
repository and task before adding production-only exclusions:

- `--exclude-file <GLOB>` excludes dedicated test/generated paths. It is
  repeatable; useful examples include `tests/**`, `*.test.*`, and `*.spec.*`.
- `--exclude-preset module-tests` excludes Rust `#[cfg(test)] mod` blocks,
  OCaml inline-test forms such as `let%test`, and JavaScript/TypeScript/TSX
  `if (import.meta.vitest)` blocks. It intentionally keeps ordinary top-level
  `describe`, `it`, and `test` calls.
- `--exclude-pattern 'LANGUAGE=PATTERN'` uses an ast-grep code pattern.
- `--exclude-query 'LANGUAGE=QUERY'` and
  `--exclude-query-file LANGUAGE=FILE` use Tree-sitter queries whose excluded
  subtree roots are captured as `@exclude`.
- Other node properties accepted by `--exclude-kind` are `named`, `extra`,
  `error`, and `missing`; never exclude both `named` and `anonymous`.

Run `<astcount-command> count <scope> --exclude-kind anonymous --by-type` when
grammar-specific type discovery would help. If the request explicitly targets
particular constructs, use `--select-type LANGUAGE=TYPE`,
`--select-pattern 'LANGUAGE=PATTERN'`, or `--select-query
'LANGUAGE=QUERY'`/`--select-query-file LANGUAGE=FILE`; Tree-sitter selectors
must capture nodes as `@select`. Selectors are globally active, ORed, and
deduplicated, unmatched languages contribute zero, and exclusions apply after
selection.

Default to the overall named-node metric for general refactoring. When a narrow
selector is justified, keep a secondary overall named-node report so reductions
in the target construct do not conceal broader structural growth.

Default to all code when excluding tests would redefine an ambiguous request.
Use a production-focused policy when the user asks to simplify production code
or the repository clearly separates product and test code. Keep the exact
scope, selectors, exclusions, language overrides, parser backend, and ignore
policy fixed for every report. Save reports outside the measured scope:

```console
<astcount-command> count <scope> <selector-and-filter-args> --json --save <best.json>
```

Use per-file counts to rank several high-value areas. Inspect their code, tests,
callers, and neighboring abstractions before choosing candidates.

## Keep digging

For each small candidate:

1. Make one readable, behavior-preserving simplification and format it.
2. Run the relevant existing tests, including any command supplied by the user.
3. Recount the identical scope and filter policy into `<candidate.json>`.
4. Run `<astcount-command> compare <best.json> <candidate.json>
   --fail-on-increase`.
5. Accept only when tests pass and named-node count decreases. Otherwise undo
   only that candidate without disturbing prior user changes.
6. Replace the best report, re-rank the remaining files, and continue.

Actively consider extracting inline test-only helpers and tests into established
test modules or files when that improves production boundaries and preserves
coverage. Do not expose internals, duplicate implementation, or weaken tests
just to cross an exclusion boundary. If an accepted candidate moves code into
excluded paths, describe it as test extraction rather than code deletion and
also compare a secondary named-node report without test exclusions when useful.

Before stopping, inspect multiple high-count areas and their surrounding
abstractions. Stop only when no credible simplification remains, a user limit is
reached, or further reduction would trade away clarity, behavior, public APIs,
runtime, or allocation. Run the broad relevant test suite once more.

Do not game the metric with minification, generated code, macros, parser tricks,
late filter changes, moved production files, ignore changes, or comment
deletion. Report the measurement policy, version/backend, tests, initial/final
counts, accepted and rejected candidates, test extractions, areas inspected,
tradeoffs, and stopping reason.
