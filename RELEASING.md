# Releasing astcount

Versions in `Cargo.toml` and all five npm manifests must match. Validate them
with:

```console
nix develop -c node scripts/release.mjs check 0.2.0
```

## npm and GitHub releases

The public npm package is `astcount`, with four optional platform packages
containing the native binaries.

npm will not configure a trusted publisher until a package exists. Bootstrap
each package once from an authenticated local npm session with publish-time
2FA. A temporary granular token with bypass-2FA also works, but should be
revoked immediately after bootstrapping rather than kept in GitHub Actions.

Push an annotated version tag matching `Cargo.toml`:

```console
git tag -a v0.2.0 -m 'astcount 0.2.0'
git push origin v0.2.0
```

The release workflow builds and smoke-tests Linux x64/arm64 and macOS
x64/arm64 binaries on native GitHub-hosted runners. Each runner packs and
installs its npm tarballs with lifecycle scripts disabled before publication.
The workflow publishes checksummed GitHub release archives, then the four
platform npm packages, then the launcher. Re-running a partially completed
release skips package versions that already exist.

After the first publication, log in with a normal npm web session and configure
trusted publishing for all five packages. `npm trust` requires npm 11.15 or
newer and does not accept a granular token that bypasses 2FA:

```console
npm login --auth-type=web
for package in \
  astcount-darwin-arm64 \
  astcount-darwin-x64 \
  astcount-linux-arm64-gnu \
  astcount-linux-x64-gnu \
  astcount
do
  npx --yes npm@11.19.0 trust github "$package" \
    --repo wokalski/astcount \
    --file release.yml \
    --allow-publish \
    --yes
  sleep 2
done
```

The workflow grants `id-token: write`, so releases then use short-lived OIDC
credentials and need no `NPM_TOKEN`. npm attaches provenance automatically.
For the strictest policy, package settings can disallow token publishing after
trusted publishing has been verified.

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
