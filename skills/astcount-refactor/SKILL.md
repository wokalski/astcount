---
name: astcount-refactor
description: Perform one bounded, behavior-preserving code refactor guided by before-and-after astcount measurements. Use for a focused structural simplification; use astcount-verified-refactor-loop instead when the user wants repeated optimization with the same deterministic test gate.
---

# Refactor once with astcount

Make one cohesive refactor that reduces Tree-sitter named-node count without
changing behavior.

## Measure

Infer the measurement scope from the request, defaulting to the repository.

Resolve one astcount command before measuring:

1. Run `command -v nix`. If it succeeds, treat the user as a Nix user and use
   `nix run github:wokalski/astcount/v0.2.0 --`.
2. Otherwise, use `bunx astcount@0.2.0` when `command -v bun` succeeds.
3. Otherwise, use `npx --yes astcount@0.2.0` when `command -v npx` succeeds.
4. If none is available, stop and tell the user that Nix, Bun, or npx is
   required.

Run the selected command with `--version`, then reuse that exact invocation for
every command below. Keep the astcount version, parser backend, scope, language
overrides, and ignore policy fixed. Save reports outside the measured scope:

```console
<astcount-command> count <scope> --exclude anonymous --json --save <before.json>
```

Use the per-file counts to choose one high-value target. Inspect the code and its
tests before editing, then make one readable, behavior-preserving simplification.
Do not broaden the task into an optimization loop.

## Verify

Format the changed code and run the relevant existing tests, including any exact
command supplied by the user. Do not weaken tests or alter public behavior
without permission. Recount the identical scope:

```console
<astcount-command> count <scope> --exclude anonymous --json --save <after.json>
<astcount-command> compare <before.json> <after.json> --fail-on-increase
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
