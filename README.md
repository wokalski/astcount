# deslop

`deslop` is a fast, polyglot syntax-tree node counter. It measures program size by
parsing source files with Tree-sitter instead of counting physical lines.

## Install

Run directly or install permanently with Nix:

```console
nix run github:wokalski/deslop -- .
nix profile install github:wokalski/deslop
```

The npm distribution uses the same native Rust binary:

```console
npm install --global @wokalski/deslop
deslop .
```

Linux glibc on x64/arm64 and macOS on Intel/Apple Silicon are supported by npm.
Building from source and Nix additionally remain available on every system in
the flake.

## Usage

```console
deslop src
deslop . --files
deslop . --threads 4
deslop . --json
deslop . --save baseline.json
deslop . --compare baseline.json --fail-on-increase
```

The default metric is the number of **named, non-extra Tree-sitter nodes**. This
excludes comments and is the closest grammar-independent approximation of AST
nodes. `--mode named` includes named extras such as comments; `--mode all` counts
the full concrete syntax tree, including punctuation. The report also includes
all three node counts, parser errors, maximum tree depth, bytes, and language.

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
AST, named, and concrete nodes plus comments, parser errors, missing nodes, tree
depth, and byte totals. It runs offline as part of `nix flake check`.

`nix flake check` also stages the Nix-built executable as an npm platform
package and invokes it through the JavaScript launcher. Release automation and
public-cache setup are documented in [`RELEASING.md`](RELEASING.md).

## What the number means

Node count is a structural size metric, not a proof of software quality. Compare
the same codebase using the same `deslop` version, language grammar, flags, and
generated/vendor-file policy. Counts from different languages or grammar
versions are not directly comparable.
