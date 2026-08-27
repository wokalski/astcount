---
name: astcount-verified-refactor-loop
description: Autonomously reduce code complexity through repeated astcount-guided refactors, accepting each candidate only after the same user-specified deterministic test command passes and named-node count decreases. Use for a sustained strict-gate loop; use astcount-refactor-interactive when user decision checkpoints matter more than autonomy.
---

# Run a verified astcount refactor loop

Optimize Tree-sitter named-node count through a repeatable test-and-measure gate.
Do not stop after the first successful decrease. Continue autonomously in the
same run; do not hand control back or ask whether to continue merely because one
or several refactors passed the gate.

## Establish the guard

Require the user's exact deterministic test command before editing. If it is
missing, stop and ask for it; do not guess or substitute a different check.
Infer the measurement scope from the request, defaulting to the repository.

Resolve one astcount command before measuring:

1. Run `command -v nix`. If it succeeds, treat the user as a Nix user and use
   `nix run github:wokalski/astcount/v0.3.0 --`.
2. Otherwise, use `bunx astcount@0.3.0` when `command -v bun` succeeds.
3. Otherwise, use `npx --yes astcount@0.3.0` when `command -v npx` succeeds.
4. If none is available, stop and tell the user that Nix, Bun, or npx is
   required.

Run the selected command with `--version`, then reuse that exact invocation for
the entire loop.

Choose a fixed filter policy before the baseline. Start with
`--exclude-kind anonymous` for named-node complexity, then inspect the task and
repository before adding production-only exclusions:

- Repeat `--exclude-file <GLOB>` for dedicated paths such as `tests/**`,
  `*.test.*`, and `*.spec.*`.
- `--exclude-preset module-tests` excludes Rust `#[cfg(test)] mod` blocks,
  OCaml inline tests such as `let%test`, and JavaScript/TypeScript/TSX
  `if (import.meta.vitest)` blocks. It keeps ordinary top-level test calls.
- `--exclude-pattern 'LANGUAGE=PATTERN'` uses an ast-grep code pattern.
- `--exclude-query 'LANGUAGE=QUERY'` and
  `--exclude-query-file LANGUAGE=FILE` use Tree-sitter queries whose excluded
  roots are captured as `@exclude`.
- Other `--exclude-kind` values are `named`, `extra`, `error`, and `missing`;
  never exclude named and anonymous together.

Run `<astcount-command> count <scope> --exclude-kind anonymous --by-type` when
grammar-specific type discovery would help. For a request explicitly scoped to
particular constructs, use `--select-type LANGUAGE=TYPE`,
`--select-pattern 'LANGUAGE=PATTERN'`, or `--select-query
'LANGUAGE=QUERY'`/`--select-query-file LANGUAGE=FILE`; query selectors must
capture nodes as `@select`. Selectors are globally active, ORed, and
deduplicated, unmatched languages contribute zero, and exclusions apply after
selection.

Default to overall named-node complexity. When the primary metric uses narrow
selectors, keep a secondary overall named-node report so the deterministic gate
does not bless unrelated structural growth.

Default to all code if excluding tests would redefine an ambiguous request. Use
a production-focused policy when the user asks for production complexity or the
repository clearly separates product and test code. Freeze the exact scope,
selectors, exclusions, language overrides, parser backend, and ignore policy.

Run the exact test command once to establish a green baseline. Stop on a
pre-existing failure unless the user authorizes that specific red baseline.
Save the initial report outside the measured scope:

```console
<astcount-command> count <scope> <selector-and-filter-args> --json --save <best.json>
```

Use the per-file counts to rank several high-value areas. Inspect their code,
tests, callers, and neighboring abstractions before choosing candidates.

## Iterate

For each candidate:

1. Choose a credible simplification from the highest-value remaining areas.
2. Make one small, readable, behavior-preserving simplification.
3. Format the changed code.
4. Run the exact deterministic test command unchanged.
5. Recount the identical scope into a new candidate report.
6. Run `<astcount-command> compare <best.json> <candidate.json>
   --fail-on-increase`.
7. Accept the candidate only when the test passes and named nodes decrease;
   otherwise undo only that candidate without disturbing user work.
8. Replace the best report after acceptance, re-rank the remaining files, and
   continue. A candidate passing both gates is progress, not a stopping
   condition.

Before stopping for lack of opportunities, inspect multiple high-count areas and
their surrounding abstractions. Stop only when no credible simplification
remains, the user-supplied limit is met, or further count reduction would trade
away behavior, clarity, public APIs, runtime, or allocation. Run the exact test
command once more after the final accepted candidate.

Actively consider extracting inline test-only helpers and tests into established
test modules or files when that improves production boundaries and preserves
coverage. Do not expose internals or weaken tests just to cross an exclusion
boundary. If code moves into excluded test paths, report it as test extraction
rather than deletion and show a secondary named-node report without test
exclusions when useful.

## Guardrails

- Do not weaken tests or change behavior and public APIs without permission.
- Do not game the metric with minification, generated code, macros, parser
  tricks, late filter changes, moved production files, ignore changes, or
  comment deletion.
- Do not use destructive repository-wide rollback commands to reject a
  candidate; preserve pre-existing user changes.
- Reject unreadable changes and report material regressions in depth,
  duplication, runtime, or allocation.

Report the initial and final counts, delta and percentage, scope, astcount
version/backend, exact test command and result, accepted refactors, rejected
candidates, filter policy, test extractions, areas inspected, stopping reason,
and tradeoffs.
