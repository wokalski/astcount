---
name: astcount-refactor-interactive
description: Collaboratively reduce code complexity with astcount while actively asking the user to choose performance, correctness, API, and test-separation tradeoffs. Use when the user wants decision checkpoints; use astcount-refactor-loop for autonomous iteration or a verified variant for one exact deterministic test gate.
---

# Refactor interactively with astcount

Use astcount to find structural simplifications, but make the user an active
partner in consequential decisions. Do not turn “interactive” into vague status
questions: inspect first, present concrete options and evidence, and ask focused
questions about real tradeoffs.

## Establish the measurement together

Infer the scope from the request. Resolve one astcount command, run it with
`--version`, and reuse that exact invocation:

1. When `command -v nix` succeeds, use
   `nix run github:wokalski/astcount/v0.3.0 --`.
2. Otherwise, use `bunx astcount@0.3.0` when Bun is available.
3. Otherwise, use `npx --yes astcount@0.3.0` when npx is available.
4. Stop if none is available.

Inspect the repository before editing, then propose a filter policy. Start with
`--exclude-kind anonymous` and explain any production-only additions:

- Repeat `--exclude-file <GLOB>` for dedicated paths such as `tests/**`,
  `*.test.*`, or `*.spec.*`.
- `--exclude-preset module-tests` excludes Rust `#[cfg(test)] mod` blocks,
  OCaml inline tests such as `let%test`, and JavaScript/TypeScript/TSX
  `if (import.meta.vitest)` blocks. It does not remove ordinary top-level test
  calls.
- Use `--exclude-pattern 'LANGUAGE=PATTERN'` for friendly ast-grep matching.
- Use `--exclude-query 'LANGUAGE=QUERY'` or
  `--exclude-query-file LANGUAGE=FILE` for precise Tree-sitter matching; query
  roots must be captured as `@exclude`.
- `--exclude-kind` also accepts `named`, `extra`, `error`, and `missing`; named
  and anonymous cannot both be excluded.

Use `<astcount-command> count <scope> --exclude-kind anonymous --by-type` to
discover grammar-specific node types when the task targets a construct. Add
selectors only with the user's agreement:

- `--select-type LANGUAGE=TYPE` selects an exact grammar node type.
- `--select-pattern 'LANGUAGE=PATTERN'` selects ast-grep match roots.
- `--select-query 'LANGUAGE=QUERY'` and
  `--select-query-file LANGUAGE=FILE` select nodes captured as `@select`.

Any selector enables global selection mode. All selectors are ORed and
deduplicated; languages without a matching selector contribute zero. Astcount
applies exclusions after selection. Prefer the overall named-node metric unless
the user wants a construct-specific objective, and track a secondary overall
named-node report when narrowing the primary metric could hide regressions.

Before the baseline, actively ask the user to confirm:

1. Whether the primary metric covers all named nodes, production code with the
   proposed test exclusions, or an explicitly selected set of constructs.
2. The correctness boundary: exact observable behavior, public API and error
   compatibility, or any explicitly allowed change.
3. The performance boundary: whether extra allocation, copying, dynamic
   dispatch, startup cost, or slower hot paths is acceptable for simpler code.
4. Whether moving inline tests and test-only helpers into dedicated test files
   is desirable when coverage and access constraints permit it.

Do not ask again for choices the user already supplied. Freeze the confirmed
scope, selectors, exclusions, language overrides, parser backend, and ignore
policy, then save the baseline outside the measured scope:

```console
<astcount-command> count <scope> <selector-and-filter-args> --json --save <best.json>
```

## Work in decision rounds

Rank high-value files and inspect code, tests, callers, and neighboring
abstractions. Present a small set of concrete candidates with expected
structural benefit, correctness risk, runtime/allocation effect, API impact,
and available tests. Recommend one and ask the user which tradeoff to take.

For an approved candidate, make a small change, format it, run relevant tests,
recount with the identical policy, and compare with `--fail-on-increase`. Keep
it only when tests pass and named nodes decrease; otherwise undo only that
candidate. Show the measured result before the next decision round.

Ask before any candidate that changes error behavior, public APIs, ordering,
precision, concurrency, allocation, asymptotic work, or hot-path performance.
When a candidate is a clearly behavior-neutral cleanup under the confirmed
policy, it may be grouped with similar low-risk work, but still report the
tradeoff evidence at the next checkpoint.

Encourage extracting test-only code when it improves boundaries, but do not
weaken coverage or expose production internals solely to lower the primary
metric. When code crosses into excluded test paths, call it test extraction,
not code deletion, and show a secondary named-node count without test
exclusions when useful.

Do not game the metric through minification, generated code, macros, parser
tricks, late filter changes, moved production files, ignore changes, or comment
deletion. Finish with the confirmed policy, decisions made, tests, counts,
accepted/rejected candidates, test extractions, unresolved opportunities, and
tradeoffs.
