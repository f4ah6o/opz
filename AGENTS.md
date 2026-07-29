# Contributor contract

This file applies to the whole repository. It is written for both human
contributors and coding agents.

## Scope

`opz` is a command-line wrapper around 1Password tooling. It locates vault
items, creates `op://` references, injects resolved values into explicitly
trusted child processes, manages 1Password Developer Environments through MCP,
and sends selected values to supported deployment tools.

`opz` is not a password manager, a general shell, or a secret cache. The
library target exists only to keep `src/main.rs` small; it is not a supported
Rust library API.

## Before changing code

- Start from current `main` and inspect open issues and pull requests before
  implementing overlapping work.
- Do not open a parallel implementation when an active PR already owns the
  scope. In particular, inspect the issue/PR pairs #38/#39 and #41/#43 before
  changing those areas.
- Keep a change focused. Security hardening may deliberately change behavior,
  but unrelated compatibility changes need a separate issue.

## Architecture and ownership

Read [docs/architecture.md](docs/architecture.md) before moving code across
modules. Dependencies flow from CLI parsing and dispatch toward domain and
process helpers; lower modules must not depend on clap command types.

- `cli`: clap declarations, parsing, dispatch, and CLI-to-domain conversion.
- `op`: 1Password item models, templates, and item CLI operations.
- `mcp`: JSON-RPC transport and Developer Environment operations.
- `environment`: native `op run` Environment delegation.
- `resolver`: item/repository resolution and metadata-only caches.
- `envfile`: env parsing, merging, rendering, and secure file replacement.
- `migration`: supported script and env migration.
- `targets`: GitHub and Cloudflare target validation and delivery.
- `doctor`: dependency, authentication, and plaintext-file diagnostics.
- `process`: child execution and external-command timeouts.
- `security`: `SecretValue`, redaction, and secret-bearing output policy.
- `skill`, `instrumentation`: bundled skill and telemetry seams.

Keep cross-module APIs `pub(crate)` or narrower. Do not turn `main_entry` into a
supported public API.

## Required checks

Use the repository recipes:

```sh
just check
just security-check
just release-check
```

`just check` runs formatting, clippy, and all hermetic tests with
`test-support`. `just security-check` runs cargo-deny, cargo-machete, and the
workflow pin check. `just release-check` adds version, package, cargo-dist,
cargo-binstall metadata, release build, and publish-dry-run checks.

Normal tests must not require a shell-script fixture or a real account. Extend
`tests/hermetic_cli.rs` and its JSON fake-tool protocol for CLI behavior.
`tests/e2e_real_op.rs` is destructive to a temporary real 1Password item and
must remain opt-in behind `OPZ_E2E=1`.

## Platform contract

Linux, macOS, and Windows are supported and tested. Code and tests must handle
platform executable suffixes, path rules, process statuses, and permission
differences. Keep Unix permission and symlink assertions behind appropriate
`cfg` gates. Do not replace the cross-platform fake-tool harness with Unix
shell scripts.

## Security contract

Follow [docs/security.md](docs/security.md). In particular:

- `opz` MUST NOT place secret values in argv or emit them in its own stdout,
  stderr, logs, traces, errors, caches, dry-run output, fixtures, snapshots, or
  repository files. An explicitly trusted child may read and print its
  environment.
- Use `SecretValue` for resolved values and the longest-match `Redactor` before
  relaying captured output or adding external output to an error.
- Deliver resolved values only through an explicitly trusted child environment
  or the documented `gh`/`wrangler` stdin protocols. Secret-bearing imports
  and notes go to `op item create` through stdin.
- Dry runs MUST NOT resolve values. Persistent env files contain `op://`
  references and preserved unrelated content, never resolved values.
- Caches may contain item and repository metadata only.
- Use distinctive canaries in security tests and assert their absence from
  every unintended channel.

Live work with 1Password Developer Environments must use the 1Password MCP
server and its authorization flow. Never ask a user to reveal a secret or add a
real secret to a test scenario.

## External-tool boundaries

- `op` is trusted for authentication, item operations, reference resolution,
  and native Environment-backed execution.
- The 1Password MCP server is trusted for Environment metadata and mutations;
  `opz` must not request Environment variable values.
- `git` supplies repository metadata only.
- On Unix, `sh` is a narrow adapter for batch environment capture and exact
  child argv forwarding; Windows launches the child directly.
- `gh` receives one secret value through stdin; `wrangler` receives the bulk
  JSON payload through stdin. Neither receives values in argv.
- `secretlint` may read candidate credential files during `doctor`; do not
  forward file content or unredacted secret-bearing diagnostics.
- A user-selected child receives resolved values in its environment and is an
  explicit trust boundary.

Treat all external output as untrusted and potentially secret-bearing.

## Compatibility and documentation

Follow [docs/compatibility.md](docs/compatibility.md). Preserve documented
commands, flags, aliases, environment variables, script-oriented stdout,
stdout/stderr routing, cache invariants, and exit semantics.

When CLI behavior changes, update all of these in the same PR:

1. clap help and parser tests;
2. `README.md`;
3. `README.ja.md`;
4. `.agents/skills/opz/SKILL.md`;
5. hermetic CLI tests for output streams and exit codes.

Update `docs/security.md` with security-contract changes and
`docs/compatibility.md` when a stable surface changes.

## Releases

Follow [docs/releasing.md](docs/releasing.md). Releases use CalVer
`YYYY.M.PATCH` and tags `vYYYY.M.PATCH`. Never bump a version or push a tag as
part of an ordinary feature PR. A release requires green checks, release notes,
version consistency, cargo-dist and Trusted Publishing verification, archive
smoke tests, and post-release cargo-binstall verification.
