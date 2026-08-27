# astcount

`astcount` is a fast, polyglot syntax-tree node counter. It measures program size by
parsing source files with Tree-sitter instead of counting physical lines.

The motivation is simple: syntax-tree node count is a rough, objective proxy for
program complexity, and it is harder to game than line or character count.
Reformatting, removing blank lines, or shortening identifiers can dramatically
change those textual metrics without simplifying the program, while leaving its
syntax tree largely unchanged. `astcount` therefore works well as a directional
benchmark for whether a refactor made code structurally simpler.

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

```console
astcount count src
astcount count . --files
astcount count . --files --stats
astcount count . --threads 4
astcount count . --exclude anonymous
astcount count . --exclude anonymous,extra
astcount count . --exclude extra,error,missing
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

Human output puts paths first and shows only the selected node count. `--stats`
adds a final line with files, bytes, wall time, aggregate parse time, throughput,
and raw parser diagnostics. JSON and saved schema-3 reports group selected and
raw counts under `nodes`, with raw property counts under `nodes.by_property`.

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

The repository also includes the [`astcount-reduce-complexity`](skills/astcount-reduce-complexity/SKILL.md)
Codex skill. Give it an exact test command and it will iteratively accept only
behavior-preserving refactors that reduce the named-node count.

## What the number means

Node count is a structural size metric, not a proof of software quality. Compare
the same codebase using the same `astcount` version, language grammar, flags, and
generated/vendor-file policy. Counts from different languages or grammar
versions are not directly comparable.
