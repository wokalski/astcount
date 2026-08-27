---
name: astcount-verified-refactor-loop
description: Iteratively reduce code complexity with astcount, accepting each small refactor only after the same user-specified deterministic test command passes and named-node count decreases. Use for guarded repeated optimization, not a single refactor.
---

# Run a verified astcount refactor loop

Optimize Tree-sitter named-node count through a repeatable test-and-measure gate.

## Establish the guard

Require the user's exact deterministic test command before editing. If it is
missing, stop and ask for it; do not guess or substitute a different check.
Infer the measurement scope from the request, defaulting to the repository.

Resolve one astcount command before measuring:

1. Run `command -v nix`. If it succeeds, treat the user as a Nix user and use
   `nix run github:wokalski/astcount/v0.2.0 --`.
2. Otherwise, use `bunx astcount@0.2.0` when `command -v bun` succeeds.
3. Otherwise, use `npx --yes astcount@0.2.0` when `command -v npx` succeeds.
4. If none is available, stop and tell the user that Nix, Bun, or npx is
   required.

Run the selected command with `--version`, then reuse that exact invocation for
the entire loop. Keep its parser backend, scope, language overrides, and ignore
policy fixed. Run the exact test command once to establish a green baseline.
Stop on a pre-existing failure unless the user authorizes that specific red
baseline. Save the initial report outside the measured scope:

```console
<astcount-command> count <scope> --exclude anonymous --json --save <best.json>
```

## Iterate

For each candidate:

1. Use per-file counts and code inspection to choose one high-value target.
2. Make one small, readable, behavior-preserving simplification.
3. Format the changed code.
4. Run the exact deterministic test command unchanged.
5. Recount the identical scope into a new candidate report.
6. Run `<astcount-command> compare <best.json> <candidate.json>
   --fail-on-increase`.
7. Accept the candidate only when the test passes and named nodes decrease;
   otherwise undo only that candidate without disturbing user work.
8. Replace the best report after acceptance, then choose the next candidate.

Stop when no credible simplification remains, the user-supplied limit is met, or
the next change would trade away clarity or behavior. Run the exact test command
once more after the final accepted candidate.

## Guardrails

- Do not weaken tests or change behavior and public APIs without permission.
- Do not game the metric with minification, generated code, macros, parser
  tricks, moved files, ignore changes, or comment deletion.
- Do not use destructive repository-wide rollback commands to reject a
  candidate; preserve pre-existing user changes.
- Reject unreadable changes and report material regressions in depth,
  duplication, runtime, or allocation.

Report the initial and final counts, delta and percentage, scope, astcount
version/backend, exact test command and result, accepted refactors, rejected
candidates, stopping reason, and tradeoffs.
