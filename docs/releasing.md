# Release procedure

`opz` uses CalVer `YYYY.M.PATCH` and immutable tags `vYYYY.M.PATCH`.
`f4ah6o/calver-action` allocates the current year/month and the next unused
patch number in `Asia/Tokyo`. The selected source commit is marked by the
movable `latest` tag; release version changes are made in a release-only commit
and are not merged back into `main`.

The repository currently pins cargo-dist to `0.31.0`. Do not combine a release
with a cargo-dist upgrade. `.github/workflows/release.yml` is generated and
then intentionally post-processed to pin actions to full commit SHAs and to
support explicit dispatch from the CalVer release workflow;
`dist-workspace.toml` permits the CI-only difference.

## Prepare

1. Start a release PR from current `main`. Confirm no other release or version
   PR is active.
2. Summarize user-visible changes and migrations in release notes. If the
   repository adopts a changelog, update it in this PR as well.
3. Do not manually bump `Cargo.toml` or `Cargo.lock` for the release. The
   CalVer workflow updates both in its release-only commit.
4. Synchronize changed behavior across clap help, `README.md`, `README.ja.md`,
   `.agents/skills/opz/SKILL.md`, and the security/compatibility documents.
5. Confirm action pins remain full SHAs with readable version comments.

Run:

```sh
just release-check
```

This includes formatting, clippy, hermetic tests, cargo-deny, cargo-machete,
workflow-pin validation, version-format validation, package-content validation,
`dist plan`, cargo-binstall/archive agreement, a release build, and
`cargo publish --locked --dry-run`.

Review `target/dist-plan.json` and confirm that release archives contain `opz`
only—not `opz-test-tool`—for the configured targets. Do not move `latest` until
the release PR is merged and all Linux, macOS, Windows, security, and cargo-dist
plan checks are green.

## Publish

From a clean, up-to-date `main`, run:

```sh
just release
```

The recipe reruns `release-check`, moves the `latest` tag to `HEAD`, and pushes
it. `.github/workflows/version.yaml` then:

1. calls the pinned `f4ah6o/calver-action` Rust release workflow;
2. allocates the next `YYYY.M.PATCH` in `Asia/Tokyo`;
3. writes that version to `Cargo.toml` and `Cargo.lock` in a release-only
   commit;
4. creates the immutable `vYYYY.M.PATCH` tag on that commit;
5. explicitly dispatches the preserved cargo-dist and crates.io workflows at
   that immutable tag.

The explicit dispatch is intentional. A tag pushed by a workflow using
`GITHUB_TOKEN` does not normally start another workflow recursively.

The dispatched release paths remain:

- `.github/workflows/release.yml` builds and publishes cargo-dist archives and
  installers;
- `.github/workflows/publish.yaml` publishes the crate through crates.io
  Trusted Publishing using GitHub OIDC.

`latest` is the only movable release selector. Never move or reuse an immutable
`vYYYY.M.PATCH` tag. Do not create a replacement release manually while either
publication workflow is running. If a job fails, preserve its logs, fix the
cause on a new source commit/version as appropriate, and follow the repository's
tag policy rather than moving a published version tag.

## Verify

After the GitHub release is published:

- Confirm the GitHub release version, immutable tag, crate metadata, and
  crates.io version agree.
- Confirm Linux `x86_64-unknown-linux-gnu`, macOS
  `x86_64-apple-darwin`, and Windows `x86_64-pc-windows-msvc` archives are
  present.
- Require the `Release smoke test` workflow to pass. The cargo-dist workflow
  dispatches it explicitly after publication because releases created with
  `GITHUB_TOKEN` do not recursively trigger workflows. It downloads the exact
  cargo-dist archives and runs `opz --version` and `opz --help` on all three
  operating systems. Use its manual `tag` input to retry a published release.
- On Linux, require the smoke workflow's bounded crates.io retry and
  `cargo binstall` check to pass with compile and quick-install fallbacks
  disabled.
- Review the published release notes and installation commands.

For a separate manual cargo-binstall verification, use an isolated root:

```sh
install_root=$(mktemp -d)
cargo binstall "opz@YYYY.M.PATCH" \
  --no-confirm \
  --force \
  --root "$install_root" \
  --no-track \
  --disable-strategies compile,quick-install
"$install_root/bin/opz" --version
"$install_root/bin/opz" --help
```

Record or link the successful CalVer allocation, GitHub release, crates.io
publish, archive smoke, and cargo-binstall checks in the release notes or
release issue.
