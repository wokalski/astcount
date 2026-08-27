# deslop

`deslop` is a fast, polyglot syntax-tree node counter. It measures program size by
parsing source files with Tree-sitter instead of counting physical lines.

```console
nix run . -- src
nix run . -- . --files
nix run . -- . --threads 4
nix run . -- . --json
nix run . -- . --save baseline.json
nix run . -- . --compare baseline.json --fail-on-increase
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
nix flake check
```

The test suite includes exact golden counts for a pinned Rust grammar, covering
AST, named, and concrete nodes plus comments, parser errors, missing nodes, tree
depth, and byte totals. It runs offline as part of `nix flake check`.

## What the number means

Node count is a structural size metric, not a proof of software quality. Compare
the same codebase using the same `deslop` version, language grammar, flags, and
generated/vendor-file policy. Counts from different languages or grammar
versions are not directly comparable.
