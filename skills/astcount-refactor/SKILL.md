---
name: astcount-refactor
description: Perform one bounded, behavior-preserving code refactor guided by before-and-after astcount measurements. Use for a focused structural simplification; use astcount-verified-refactor-loop instead when the user wants repeated optimization with the same deterministic test gate.
---

# Refactor once with astcount

Make one cohesive refactor that reduces Tree-sitter named-node count without
changing behavior.

## Measure

Infer the measurement scope from the request, defaulting to the repository. Use
`astcount`, or `nix run github:wokalski/astcount --` if it is unavailable. Keep
the astcount version, parser backend, scope, language overrides, and ignore
policy fixed. Save reports outside the measured scope:

```console
astcount count <scope> --exclude anonymous --json --save <before.json>
```

Use the per-file counts to choose one high-value target. Inspect the code and its
tests before editing, then make one readable, behavior-preserving simplification.
Do not broaden the task into an optimization loop.

## Verify

Format the changed code and run the relevant existing tests, including any exact
command supplied by the user. Do not weaken tests or alter public behavior
without permission. Recount the identical scope:

```console
astcount count <scope> --exclude anonymous --json --save <after.json>
astcount compare <before.json> <after.json> --fail-on-increase
```

Keep the refactor only when verification passes and named-node count decreases.
If it fails either gate, undo only this skill's edits without disturbing prior
user changes, and report the rejected candidate.

Do not game the metric with minification, generated code, macros, parser tricks,
moved files, ignore changes, or comment deletion. Reject changes that make the
code harder to understand or materially regress depth, duplication, runtime, or
allocation.

Report the scope, version/backend, test command and result, before/after counts,
delta and percentage, the refactor, and any tradeoff.
