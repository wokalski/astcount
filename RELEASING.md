# Releasing astcount

Versions in `Cargo.toml` and all five npm manifests must match. Validate them
with:

```console
nix develop -c node scripts/release.mjs check 0.1.0
```

## npm and GitHub releases

The public npm package is `astcount`, with four optional platform packages
containing the native binaries.

The first npm publication needs a granular automation token stored as the
GitHub Actions secret `NPM_TOKEN`. Push an annotated version tag matching
`Cargo.toml`:

```console
git tag -a v0.1.0 -m 'astcount 0.1.0'
git push origin v0.1.0
```

The release workflow builds and smoke-tests Linux x64/arm64 and macOS
x64/arm64 binaries on native GitHub-hosted runners. Each runner packs and
installs its npm tarballs with lifecycle scripts disabled before publication.
The workflow publishes checksummed GitHub release archives, then the four
platform npm packages, then the launcher. Re-running a partially completed
release skips package versions that already exist.

After the first publication, configure npm trusted publishing for all five
packages using GitHub repository `wokalski/astcount`, workflow `release.yml`, and
the `npm publish` permission. The workflow already grants `id-token: write`, so
`NPM_TOKEN` can then be removed. npm will attach provenance automatically.

## Public Nix cache

Create a public Cachix cache, preferably named `astcount`, and generate a
per-cache write token. In the GitHub repository settings add:

- Actions variable `CACHIX_CACHE_NAME=astcount`
- Actions secret `CACHIX_AUTH_TOKEN=<per-cache write token>`

CI detects the variable automatically, substitutes from the public cache, and
pushes Nix builds from `main` when the token is available. Pull requests use the
cache read-only. Without those settings CI still builds normally, so forks do
not need Cachix credentials.

Once the cache exists, users can opt into it with:

```console
nix run nixpkgs#cachix -- use astcount
nix run github:wokalski/astcount -- .
```
