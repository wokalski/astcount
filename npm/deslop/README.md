# @wokalski/deslop

Native npm distribution of [deslop](https://github.com/wokalski/deslop), a
fast, polyglot Tree-sitter AST node counter.

```console
npm install --global @wokalski/deslop
deslop .
```

The package selects a platform-specific native binary at runtime. Linux glibc
on x64 and arm64, plus macOS on Intel and Apple Silicon, are supported.
