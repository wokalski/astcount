# astcount

Native npm distribution of [astcount](https://github.com/wokalski/astcount), a
fast, polyglot Tree-sitter node counter.

```console
npm install --global astcount
astcount .
```

The package selects a platform-specific native binary at runtime. Linux glibc
on x64 and arm64, plus macOS on Intel and Apple Silicon, are supported.
