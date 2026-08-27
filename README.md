# astcount

[![skills.sh](https://skills.sh/b/wokalski/astcount)](https://skills.sh/wokalski/astcount)

`astcount` measures source code through its Tree-sitter syntax tree. A syntax
tree keeps the structure of a program while ignoring whitespace, line wrapping,
and identifier length. Counting its nodes gives a rough estimate of how much
code is actually there, rather than how much space the text takes up.

## Agent usage

Install all three skills globally for Codex with the
[skills.sh CLI](https://skills.sh/wokalski/astcount):

```console
bunx skills add wokalski/astcount --skill '*' --agent codex --global --yes
```

Then open Codex in the repository you want to change and include the skill and
task in the same prompt. These are Codex prompts, not shell commands:

```text
$astcount-refactor-interactive Inspect this repository, propose refactors, and ask me about correctness and performance tradeoffs.
$astcount-refactor-loop Refactor this repository autonomously until credible structural gains are exhausted.
$astcount-verified-refactor-loop Refactor src deeply. Use `bun test` as the exact deterministic test command and keep digging until no credible structural improvement remains.
```

- [`astcount-refactor-interactive`](skills/astcount-refactor-interactive/SKILL.md)
  asks before correctness, API, or performance tradeoffs.
- [`astcount-refactor-loop`](skills/astcount-refactor-loop/SKILL.md) keeps
  simplifying autonomously.
- [`astcount-verified-refactor-loop`](skills/astcount-verified-refactor-loop/SKILL.md)
  additionally gates every candidate with one exact test command.

All three freeze the astcount policy before measuring and understand selectors,
test exclusions, ast-grep patterns, and Tree-sitter queries.

### Try without installing

`skills use` can instead download one skill and open a temporary Codex session:

```console
bunx skills use wokalski/astcount@astcount-refactor-interactive --agent codex
bunx skills use wokalski/astcount@astcount-refactor-loop --agent codex
bunx skills use wokalski/astcount@astcount-verified-refactor-loop --agent codex
```

The temporary session loads the skill, then waits for your refactoring request.

## Install

Run the native binary with Bun:

```console
bunx astcount .
```

The package downloads the matching native Rust binary without a lifecycle
script. It supports Linux glibc and macOS on x64 and arm64.

Run directly or install permanently with Nix, using the public Cachix cache:

```console
nix run --extra-substituters https://astcount.cachix.org --extra-trusted-public-keys astcount.cachix.org-1:NgwAPl0WX9xB3qatDahUC8T0R9jcEuwOFhgdrwV/lk8= github:wokalski/astcount -- .
nix profile install --extra-substituters https://astcount.cachix.org --extra-trusted-public-keys astcount.cachix.org-1:NgwAPl0WX9xB3qatDahUC8T0R9jcEuwOFhgdrwV/lk8= github:wokalski/astcount
```

## Usage

Count the current directory, or name a narrower source tree:

```console
astcount .
astcount count src
```

Add a per-file breakdown when you want to find the largest files. Rows are
sorted by selected node count, with the largest files last. `--stats` adds the
operational summary below the table:

```console
astcount count . --files
astcount count . --files --stats
```

Use `--stream` when seeing results immediately matters more than sorting. Human
rows are printed as parsing completes; combined with `--json`, each line is a
JSON file event followed by one summary event:

```console
astcount count . --stream
astcount count . --stream --json
```

Parsing is parallel by default. Override the worker count when needed:

```console
astcount count . --threads 4
```

The default metric includes every Tree-sitter node. Exclude node kinds or
properties to focus the measurement:

```console
astcount count . --exclude-kind anonymous
astcount count . --exclude-kind anonymous,extra
astcount count . --exclude-kind extra,error,missing
```

Discover the grammar-specific types in the final selected population with
`--by-type`. The histogram is sorted from smallest to largest and includes the
language plus whether each type is named or anonymous:

```console
astcount count src --exclude-kind anonymous --by-type
```

Select exact grammar types when measuring particular constructs:

```console
astcount count . --select-type rust=function_item
astcount count . --select-type rust=let_declaration --select-type javascript=variable_declarator
```

Ast-grep selectors are friendlier when a source-shaped construct is easier to
describe than its grammar type. Each complete match contributes its root node,
not the entire subtree:

```console
astcount count . --select-pattern 'rust=let $NAME = $VALUE;'
astcount count . --select-pattern 'javascript=const $NAME = $VALUE'
```

Tree-sitter selector queries are the precise alternative. Nodes captured as
`@select` enter the selected population; larger queries can live in files:

```console
astcount count . --select-query 'rust=(identifier) @select'
astcount count . --select-query-file rust=queries/public-functions.scm
```

Exclude entire files with repeatable globs. Globs are relative to the current
directory; a pattern without `/` matches that basename at any depth:

```console
astcount count . --exclude-file 'tests/**'
astcount count . --exclude-file '*.test.*' --exclude-file '*.spec.*'
```

Use the built-in module-test preset to remove conventional in-source tests from
Rust, OCaml, JavaScript, and TypeScript counts:

```console
astcount count . --exclude-preset module-tests
```

For project-specific syntax, ast-grep code patterns are the concise option.
Prefix each pattern with its Tree-sitter language name:

```console
astcount count . --exclude-pattern 'rust=mod $NAME { $$$BODY }'
astcount count . --exclude-pattern 'javascript=describe($NAME, $CALLBACK)'
```

Tree-sitter queries provide the precise escape hatch. Every subtree captured as
`@exclude` is omitted. Put larger queries in a file to avoid shell quoting:

```console
astcount count . --exclude-query 'rust=(function_item) @exclude'
astcount count . --exclude-query-file rust=queries/generated-code.scm
```

Use JSON for automation, or save two complete reports and compare them. The
comparison can fail CI when the selected node count grows:

```console
astcount count . --json
astcount count . --save before.json
astcount count . --save after.json
astcount compare before.json after.json --fail-on-increase
astcount compare before.json after.json --json
```

Bare `astcount` defaults to `astcount count .`. For compatibility and quick
interactive use, count options and paths may also omit the `count` subcommand,
so `astcount src --exclude-kind anonymous` is equivalent to the explicit form.
Language is detected from filenames, extensions, and shebangs; use
`--language rust` when detection is ambiguous. Directory walks respect ignore
files such as `.gitignore`.

### Rules worth knowing

- Any selector enables selection mode. Selectors are ORed, overlaps count once,
  and languages without a matching selector contribute zero.
- `--select-type` uses exact, grammar-specific names. Unknown types are rejected;
  use `--by-type` to discover them.
- Ast-grep selectors count match roots. Tree-sitter selector queries capture
  nodes as `@select`.
- Exclusions apply after selection. Ast-grep matches and Tree-sitter captures
  named `@exclude` remove complete subtrees.
- `--exclude-kind anonymous` means named nodes only. Named and anonymous cannot
  both be excluded; parser diagnostics remain raw.
- `module-tests` covers Rust `#[cfg(test)] mod`, OCaml inline-test forms, and
  JavaScript/TypeScript/TSX `if (import.meta.vitest)` blocks. Use file globs for
  ordinary test files.

Tree-sitter types are language-specific, and whitespace usually is not a node.
“All” means all emitted tree nodes, not all tokens or bytes. Saved reports record
the complete selector/exclusion policy, and `compare` rejects incompatible
reports. Schema-3 reports remain readable.

## Development

```console
nix develop -c cargo test
nix develop -c cargo clippy --all-targets -- -D warnings
nix develop -c node scripts/release.mjs check 0.3.0
nix flake check
```

Release automation and public-cache setup are in
[`RELEASING.md`](RELEASING.md).

## What the number means

Node count is a structural size metric, not a proof of software quality. Compare
the same codebase using the same `astcount` version, language grammar, flags, and
generated/vendor-file policy. Counts from different languages or grammar
versions are not directly comparable.
