# astcount

`astcount` is a fast, polyglot syntax-tree node counter. It measures program size by
parsing source files with Tree-sitter instead of counting physical lines.

## Install

Run directly or install permanently with Nix:

```console
nix run github:wokalski/astcount -- .
nix profile install github:wokalski/astcount
```

The npm distribution uses the same native Rust binary:

```console
npm install --global astcount
astcount .
```

Linux glibc on x64/arm64 and macOS on Intel/Apple Silicon are supported by npm.
Building from source and Nix additionally remain available on every system in
the flake.

## Usage

```console
astcount src
astcount . --files
astcount . --threads 4
astcount . --require named
astcount . --require named --exclude extra
astcount . --require error
astcount . --json
astcount . --save baseline.json
astcount . --compare baseline.json --fail-on-increase
```

By default, `astcount` counts every node emitted in Tree-sitter's concrete syntax
tree, including its root. It does not invent an `AST` category. Selection is a
conjunction of Tree-sitter's per-node boolean properties:

- `--require named` counts nodes for which `is_named()` is true.
- `--require anonymous` counts nodes for which `is_named()` is false; equivalently,
  use `--exclude named`. `--exclude anonymous` is equivalent to `--require named`.
- `--require extra` counts nodes for which `is_extra()` is true.
- `--require error` counts `ERROR` nodes.
- `--require missing` counts recovery tokens inserted by the parser.
- `--exclude PROPERTY` requires that property to be false. For example,
  `--require named --exclude extra` is the commonly used “named, non-extra”
  approximation, but only when explicitly requested.

Both flags may be repeated or comma-separated. With no predicates, the selected
count is Tree-sitter's `descendant_count()` for the root. Properties overlap: an
extra comment can also be named. JSON and saved reports always contain the
selected count plus separate `total_nodes`, `named_nodes`, `extra_nodes`,
`error_nodes`, and `missing_nodes` totals.

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
nix develop -c node scripts/release.mjs check 0.1.0
nix flake check
```

The test suite includes exact golden counts for a pinned Rust grammar, covering
total, named, extra, anonymous, error, and missing selections plus tree depth and
byte totals. It runs offline as part of `nix flake check`.

`nix flake check` also stages the Nix-built executable as an npm platform
package and invokes it through the JavaScript launcher. Release automation and
public-cache setup are documented in [`RELEASING.md`](RELEASING.md).

## What the number means

Node count is a structural size metric, not a proof of software quality. Compare
the same codebase using the same `astcount` version, language grammar, flags, and
generated/vendor-file policy. Counts from different languages or grammar
versions are not directly comparable.
