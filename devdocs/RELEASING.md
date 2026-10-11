# Releases

The Git tag supplies the release version; committed package versions stay at
`0.0.0`. One commit produces seven binary downloads and the `aics` crate, using
the same release process as [tuisv](https://github.com/mevanlc/tuisv) and
[renrs](https://github.com/mevanlc/renrs).

## Normal releases

Merge changes into `main`, let CI pass, then tag the intended commit:

```sh
git tag -a v0.6.0 -m 'Release v0.6.0'
git push origin v0.6.0
```

Use a new version for each publication. Tags must be `vX.Y.Z` or a prerelease
such as `v0.6.0-rc.1`; build metadata is not supported. Prereleases publish to
crates.io and become GitHub prereleases without replacing the stable latest
release.

The workflow stamps the version, runs CI, builds every target, stages the complete
GitHub draft, publishes the verified crate, and finally publishes the draft.
Immutable releases lock the tag and assets and provide GitHub's release
attestation. GitHub's generated source archives retain the committed `0.0.0`
manifest; the attached `.crate` contains the stamped version.

## First crates.io publication

The repository's immutable-release setting is enabled. Its `crates-io` Actions
environment has no required reviewers, allowing automatic tag releases.

The first publication needs a short-lived crates.io API token with permission to
create and publish `aics`. Log into crates.io and verify your email, then create
the token and add it through the hidden prompt:

```sh
gh secret set CARGO_REGISTRY_TOKEN --repo mevanlc/aics --env crates-io
```

Never put the token in source, command arguments, logs, or chat. After the first
publication:

1. Add a GitHub trusted publisher in the crate's crates.io settings: owner
   `mevanlc`, repository `aics`, workflow `release.yml`, environment `crates-io`.
2. Verify the token exchange without publishing:

   ```sh
   gh workflow run release.yml --repo mevanlc/aics --ref main -f mode=verify-oidc
   ```

3. After that run passes, delete the bootstrap secret and revoke its crates.io
   token if it was dedicated to this bootstrap. Keep shared tokens valid for
   other projects that still use them.

The workflow selects the bootstrap secret when present and otherwise exchanges
GitHub OIDC credentials for a temporary token. Builds receive no publishing
credentials. A matching crate already in the registry is not uploaded again.

## Validation without publishing

Pull requests and pushes to `main` run quality checks, all seven enabled target builds,
and crate packaging/install checks. The Release workflow's manual `build` mode
runs these checks with a stamped version and saves Actions artifacts:

```sh
gh workflow run release.yml --repo mevanlc/aics --ref main \
  -f mode=build -f version=0.6.0
```

Manual builds cannot create GitHub releases or publish crates.

Local checks use isolated settings, cache, and index directories:

```sh
export AICS_CONFIG_ROOT="$PWD/target/ci-state/config"
export AICS_CACHE_ROOT="$PWD/target/ci-state/cache"
export AICS_DATA_ROOT="$PWD/target/ci-state/data"
cargo fmt --all --check
cargo clippy --locked --all-targets --all-features -- -D warnings
cargo nextest run --locked --all-targets --all-features
uv run scripts/test_release.py
actionlint
cargo publish --dry-run --locked --allow-dirty
```

Run helpers from the repository root. Test stamping in a disposable checkout:
`uv run scripts/release.py stamp 0.6.0` edits the manifest and the local package's
lockfile entry, checking that dependency resolution stays unchanged.

## Platforms and package checks

| Platform | Architectures | Validation |
| --- | --- | --- |
| Linux, static musl | x64, ARM64 | Native tests, static-link check, download smoke test |
| macOS 11+ | x64, ARM64 | Native tests and download smoke test |
| Windows | x64, ARM64 | Native tests and download smoke test |
| FreeBSD 15.1+ (disabled) | x64, ARM64 | Retained VM setup; no builds, tests, or required release assets |
| Android / Termux | ARM64, API 24+ | NDK build; ELF architecture, interpreter, and library checks |

Every desktop/server download runs `--help`, `--version`, and `--manpage`, then
indexes and searches a bundled Claude fixture with isolated settings and session
roots. Android execution is not covered by hosted CI.

FreeBSD is parked: both entries in `scripts/release-targets.json` have
`"enabled": false`. Disabled targets are omitted from the CI matrix and release
manifest, and their artifacts are not required by release completeness checks.
The VM steps remain in place. Remove `"enabled": false` to re-enable either target.

When enabled, FreeBSD x64 uses nextest. FreeBSD ARM64 uses `cargo test` because
nextest does not publish a binary for that target. Rustup also has no FreeBSD ARM64
installer, so that VM installs the pinned official standalone Rust distribution. Android uses
NDK 27.3.13750724; toolchain versions and action revisions are pinned in the
workflows and `rust-toolchain.toml`.

FreeBSD jobs cache the prepared VM and Cargo dependencies. Their test builds omit
debug information to reduce compilation time and memory use under emulation;
release builds retain Cargo's normal release settings.

Crate validation compiles the packaged source, checks publishing with a dry run,
installs from the unpacked crate, and runs the same isolated search. The package
includes compile-time documentation and test fixtures. Before authenticating,
the publishing job recreates the crate and compares its SHA-256 with the verified
CI artifact. `cargo publish --no-verify` skips only compilation already performed
by CI.

## Downloads and verification

Downloads use `aics-vVERSION-TARGET.tar.gz`, or `.zip` for Windows. Each contains
a matching top-level directory with `aics`, README, MIT license, command
documentation, and example configuration. Each release has 10 assets:

- Seven binary archives.
- `aics-VERSION.crate`: the exact crate published to crates.io.
- `release-manifest.json`: source commit, Rust toolchain, targets, and hashes.
- `SHA256SUMS`: hashes of the archives, crate, and manifest.

Download into an empty directory and verify:

```sh
gh release download v0.6.0 --repo mevanlc/aics
sha256sum --check SHA256SUMS
```

On macOS use `shasum -a 256 --check SHA256SUMS`; on Windows compare
`Get-FileHash -Algorithm SHA256` output with `SHA256SUMS`. `gh release verify` and
`gh release verify-asset` additionally verify the immutable release attestation.

## Recovery

Rerun failed jobs in the original Actions run. Successful build artifacts remain
available for 30 days, and the assembled release bundle for 90 days.

- A failure before final publication leaves the draft and uploaded assets intact.
- Existing assets must match their hashes; only missing assets are uploaded.
  Differing or unexpected assets stop the workflow without replacing anything.
- If the crate was uploaded before a later failure, a retry checks its registry
  checksum and skips the upload only when it matches.
- If Cargo times out after an upload, the helper polls the registry for up to
  five minutes. A retry reconciles a publication that appeared later.
- Retrying a completed matching release succeeds without changing it.

Fix application bugs with a new version. Do not move tags or replace published
releases. If a rebuild differs from an existing asset, use the original run's
artifacts: pinned tools and normalized archives do not freeze hosted runner
images.

## Maintaining this setup

Update Rust in `rust-toolchain.toml`; both workflows read that pin. Update uv,
Python, nextest, actionlint, Android NDK, and VM pins deliberately, then run the
complete manual build. Dependabot proposes pinned action updates monthly.
Enabled entries in `scripts/release-targets.json` define both the build matrix and
expected asset set. Keep compile-time files and fixtures in the Cargo package's
`include` list.
