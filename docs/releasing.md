# Release procedure

`opz` uses CalVer `YYYY.M.PATCH` and tags `vYYYY.M.PATCH`. Use the current year
and month and the next unused patch number. The crate version and tag must
match exactly.

The repository currently pins cargo-dist to `0.31.0`. Do not combine a release
with a cargo-dist upgrade. `.github/workflows/release.yml` is generated and
then intentionally post-processed to pin actions to full commit SHAs;
`dist-workspace.toml` permits that CI-only difference.

## Prepare

1. Start a release PR from current `main`. Confirm no other release or version
   PR is active.
2. Summarize user-visible changes and migrations in release notes. If the
   repository adopts a changelog, update it in this PR as well.
3. Update the version in both `Cargo.toml` and the workspace package entry in
   `Cargo.lock`. Do not change dependency versions incidentally.
4. Synchronize changed behavior across clap help, `README.md`, `README.ja.md`,
   `.agents/skills/opz/SKILL.md`, and the security/compatibility documents.
5. Confirm action pins remain full SHAs with readable version comments.

Run:

```sh
just release-check
```

This includes formatting, clippy, hermetic tests, cargo-deny, cargo-machete,
workflow-pin validation, version validation, package-content validation,
`dist plan`, cargo-binstall/archive agreement, a release build, and
`cargo publish --locked --dry-run`.

Review `target/dist-plan.json` and confirm that release archives contain `opz`
only—not `opz-test-tool`—for the configured targets. Do not push a tag until
the release PR is merged and all Linux, macOS, Windows, security, and
cargo-dist plan checks are green.

## Publish

From a clean, up-to-date `main`, run:

```sh
just release
```

That recipe reruns `release-check`, creates `v<version>`, and pushes the tag.
The tag starts two preserved release paths:

- `.github/workflows/release.yml` builds and publishes cargo-dist archives and
  installers;
- `.github/workflows/publish.yaml` publishes the crate through crates.io
  Trusted Publishing using GitHub OIDC.

Do not create a replacement release manually while either workflow is running.
If a job fails, preserve its logs, fix the cause on a new commit/version as
appropriate, and follow the repository's policy for tags rather than moving a
published tag.

## Verify

After the GitHub release is published:

- Confirm the GitHub release version, tag, and crates.io version agree.
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

Record or link the successful release, crates.io publish, archive smoke, and
cargo-binstall checks in the release notes or release issue.
