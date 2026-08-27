---
name: astcount-reduce-complexity
description: Reduce code complexity by lowering Tree-sitter named-node counts with astcount while preserving behavior. Use for structural simplification, smaller-AST refactoring, or objective complexity benchmarking. Require the user's exact test command.
---

# Reduce complexity with astcount

Optimize named-node count subject to the user-provided tests.

## Before editing

If the user omitted the exact test command, stop and ask for it. Never guess. Infer the measurement scope from the request, defaulting to the repository.

Use `astcount`, or `nix run github:wokalski/astcount --` if unavailable. Keep its version, parser backend, scope, language overrides, and ignore policy fixed.

Run the tests. Stop on a pre-existing failure unless the user authorizes a red baseline. Save the initial report outside the measured scope:

```console
astcount count <scope> --require named --json --save <before.json>
```

## Iterate

1. Use per-file JSON counts to choose a high-value target.
2. Make one small, behavior-preserving simplification.
3. Format the changed code and run the exact test command.
4. Recount the identical scope with `--require named` and save a candidate report.
5. Accept only if tests pass and named nodes decrease; otherwise revert only that candidate without disturbing user work.
6. Update the current-best report and repeat while credible improvements remain.

Use `astcount compare <best.json> <candidate.json> --fail-on-increase` as a guard. Run the full tests after the final accepted change.

## Guardrails

- Do not weaken tests or change behavior and public APIs without permission.
- Do not game the metric with minification, generated code, macros, parser tricks, moved files, ignore changes, or comment deletion.
- Reject unreadable changes and report material regressions in depth, duplication, runtime, or allocation.

## Finish

Report initial and final counts, delta and percentage, scope, `astcount` version/backend, exact test command and result, accepted refactors, rejected candidates, and tradeoffs.
