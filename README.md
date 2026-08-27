# astcount

[![skills.sh](https://skills.sh/b/wokalski/astcount)](https://skills.sh/wokalski/astcount)

`astcount` is a fast, polyglot syntax-tree node counter. It measures program size by
parsing source files with Tree-sitter instead of counting physical lines.

The motivation is simple: syntax-tree node count is a rough, objective proxy for
program complexity, and it is harder to game than line or character count.
Reformatting, removing blank lines, or shortening identifiers can dramatically
change those textual metrics without simplifying the program, while leaving its
syntax tree largely unchanged. `astcount` therefore works well as a directional
benchmark for whether a refactor made code structurally simpler.

## Agent usage

Install both skills globally for Codex with the
[skills.sh CLI](https://skills.sh/wokalski/astcount):

```console
bunx skills add wokalski/astcount --skill '*' --agent codex --global --yes
```

Then open Codex in the repository you want to change and include the skill and
task in the same prompt. These are Codex prompts, not shell commands:

```text
$astcount-refactor Refactor this repository.
$astcount-verified-refactor-loop Refactor src deeply. Use `bun test` as the exact deterministic test command and keep digging until no credible structural improvement remains.
```

[`astcount-refactor`](skills/astcount-refactor/SKILL.md) keeps digging through
behavior-preserving refactors until credible structural gains are exhausted.
[`astcount-verified-refactor-loop`](skills/astcount-verified-refactor-loop/SKILL.md)
does the same behind a strict gate: every candidate must pass the user's exact
deterministic test command and lower the named-node count before it is accepted.
Both skills detect Nix users with `command -v nix`; otherwise they run the pinned
release through Bun, with npx as a fallback.

### Try without installing

`skills use` can instead download one skill and open a temporary Codex session:

```console
bunx skills use wokalski/astcount@astcount-refactor --agent codex
bunx skills use wokalski/astcount@astcount-verified-refactor-loop --agent codex
```

The new session starts with the selected skill loaded and then waits for the
actual refactoring request. This is useful for a quick trial; installation is
the smoother workflow for repeated use.

## Install

Run the native binary with Bun:

```console
bunx astcount .
```

The registry package selects and downloads the matching native Rust binary
without running a lifecycle script. Linux glibc on x64/arm64 and macOS on
Intel/Apple Silicon are supported.

Run directly or install permanently with Nix, using the public Cachix cache:

```console
nix run --extra-substituters https://astcount.cachix.org --extra-trusted-public-keys astcount.cachix.org-1:NgwAPl0WX9xB3qatDahUC8T0R9jcEuwOFhgdrwV/lk8= github:wokalski/astcount -- .
nix profile install --extra-substituters https://astcount.cachix.org --extra-trusted-public-keys astcount.cachix.org-1:NgwAPl0WX9xB3qatDahUC8T0R9jcEuwOFhgdrwV/lk8= github:wokalski/astcount
```

Building from source and Nix additionally remain available on every system in
the flake.

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

Parsing uses the machine's available parallelism automatically. Override the
worker count when you need to reserve CPU or make completion order reproducible:

```console
astcount count . --threads 4
```

The default metric includes every Tree-sitter node. Exclude node kinds or
properties to focus the measurement:

```console
astcount count . --exclude anonymous
astcount count . --exclude anonymous,extra
astcount count . --exclude extra,error,missing
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
so `astcount src --exclude anonymous` is equivalent to the explicit form.

By default, `astcount` counts every node emitted in Tree-sitter's concrete syntax
tree, including its root. Remove node kinds or properties with repeatable or
comma-separated `--exclude` values:

- `--exclude anonymous` counts named nodes only.
- `--exclude named` counts anonymous nodes only.
- `--exclude extra` removes extra nodes such as comments.
- `--exclude error` and `--exclude missing` remove parser-recovery nodes from the
  selected count.

Excluding both `named` and `anonymous` is rejected because it would remove every
node. Properties overlap: an extra comment can also be named. Parser error and
missing-node diagnostics remain raw and unfiltered even when those properties
are excluded from the selected complexity count.

Human output puts paths first and shows only the selected node count. With
`--files`, rows are buffered and sorted from smallest to largest by that count;
`--stream` instead emits them in parser-completion order. `--stats` adds a final
line with files, bytes, wall time, aggregate parse time, throughput, and raw
parser diagnostics. JSON and saved schema-3 reports group selected and raw
counts under `nodes`, with raw property counts under `nodes.by_property`.
`--stream --json` switches stdout to JSONL: zero or more `file` events followed
by one `summary` event.

Tree-sitter also exposes each node's grammar-specific kind/name and id, source
range, child counts, fields, parse states, and whether a node or subtree changed.
Those are useful for inspection and queries, but they are not universal node
classes, so `astcount` does not silently turn them into complexity categories.
Whitespace usually is not represented as a node at all; “all” means all emitted
tree nodes, not all source tokens or bytes.

Language detection uses filenames, extensions, and shebangs. The language pack
is pinned at version 1.15.8 and knows 371 Tree-sitter grammars. It downloads a
platform parser bundle on first use, then reuses it from the local cache. Use
`--language rust` (or another pack language name) when detection is ambiguous.

Directories are walked recursively and respect hidden-file and ignore rules by
default, including `.gitignore`. Multiple file and directory arguments are
accepted, so shell globs work naturally. Parsing is parallel; files are queued
largest-first to keep oversized generated or minified sources from becoming a
single-worker tail. `--threads 0` (the default) uses the machine's available
parallelism.

## Development

```console
nix develop -c cargo test
nix develop -c cargo clippy --all-targets -- -D warnings
nix develop -c node scripts/release.mjs check 0.2.0
nix flake check
```

The test suite includes exact golden counts for a pinned Rust grammar, covering
named, anonymous, extra, error, and missing exclusions plus tree depth and byte
totals. It runs offline as part of `nix flake check`.

`nix flake check` also packs the Nix-built executable as an npm platform
package, installs the launcher and native package from their tarballs with
lifecycle scripts disabled, and invokes the installed command. Release
automation and public-cache setup are documented in
[`RELEASING.md`](RELEASING.md).

## What the number means

Node count is a structural size metric, not a proof of software quality. Compare
the same codebase using the same `astcount` version, language grammar, flags, and
generated/vendor-file policy. Counts from different languages or grammar
versions are not directly comparable.
