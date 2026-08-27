---
name: astcount-refactor
description: Deeply refactor a codebase through repeated behavior-preserving simplifications guided by astcount measurements and relevant tests, continuing until no credible structural improvement remains. Use for sustained complexity reduction; use astcount-verified-refactor-loop instead when the same user-specified deterministic test command must gate every candidate.
---

# Refactor deeply with astcount

Keep finding and applying readable, behavior-preserving simplifications that
reduce Tree-sitter named-node count. Do not stop after the first successful
decrease. Continue autonomously in the same run; do not hand control back or ask
whether to continue merely because one or several refactors succeeded.

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
<astcount-command> count <scope> --exclude anonymous --json --save <best.json>
```

Use the per-file counts to rank several high-value areas. Inspect their code,
tests, callers, and neighboring abstractions before deciding which candidates
are genuine simplifications rather than metric tricks.

## Keep digging

Work through small candidates so each one can be accepted or rejected cleanly:

1. Choose a credible simplification from the highest-value remaining areas.
2. Make the change and format the affected code.
3. Run the relevant existing tests, including any exact command supplied by the
   user. Do not weaken tests or alter public behavior without permission.
4. Recount the identical scope into a candidate report.
5. Compare the best accepted report with the candidate:

```console
<astcount-command> count <scope> --exclude anonymous --json --save <candidate.json>
<astcount-command> compare <best.json> <candidate.json> --fail-on-increase
```

6. Accept the candidate only when its tests pass and named-node count decreases.
   Otherwise, undo only that candidate without disturbing prior user changes.
7. Replace the best report with an accepted candidate, re-rank the remaining
   files, and continue. A successful candidate is progress, not a stopping
   condition.

Before stopping for lack of opportunities, inspect multiple high-count areas and
their surrounding abstractions. Stop only when no credible simplification
remains, the user-supplied limit is reached, or further count reduction would
trade away behavior, clarity, public APIs, runtime, or allocation. Run the broad
relevant test suite once more after the final accepted candidate.

Do not game the metric with minification, generated code, macros, parser tricks,
moved files, ignore changes, or comment deletion. Reject changes that make the
code harder to understand or materially regress depth, duplication, runtime, or
allocation.

Report the scope, version/backend, test commands and results, initial and final
counts, delta and percentage, accepted refactors, rejected candidates, areas
inspected, stopping reason, and tradeoffs.
