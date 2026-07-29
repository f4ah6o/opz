# Architecture

`opz` is a CLI application. `src/main.rs` only calls the doc-hidden
`opz::main_entry`. The library target is an internal organization mechanism,
not a supported Rust API.

## Dependency direction

Control flows from CLI orchestration to small domain and process helpers:

```text
main -> cli/dispatch -> domain modules -> process/security helpers
```

Lower modules accept small inputs such as `ItemContext`,
`GitHubSecretTarget`, and `CloudflareSecretTarget`. They do not import clap
command types. Cross-module visibility stays `pub(crate)` or narrower.

## Module ownership

| Module | Owns |
| --- | --- |
| `cli` | clap declarations, parsing, command dispatch, and conversion to domain inputs |
| `op` | 1Password item models, templates, item mutations, and git remote parsing used by item operations |
| `mcp` | line-oriented JSON-RPC stdio transport and Developer Environment operations |
| `environment` | native `op run` Environment delegation and capability detection |
| `resolver` | direct/fuzzy item lookup, repository auto-detection, and metadata-only caches |
| `envfile` | env parsing, merge/render rules, temporary references, and persistent-file replacement |
| `migration` | `package.json`, Just, shell-script, and env migration |
| `targets` | GitHub/Cloudflare validation, argv construction, stdin delivery, and redacted output relay |
| `doctor` | required/optional tool checks, authentication checks, credential-file discovery, and rendering |
| `process` | child command construction, secret environment injection, captured commands, and timeouts |
| `security` | `SecretValue`, longest-match `Redactor`, and masked create output |
| `skill` | compile-time bundled `.agents/skills/opz/SKILL.md` |
| `instrumentation` | telemetry seam; currently records no external telemetry |

Shared unit coverage is in `src/tests.rs`; narrowly scoped module tests live
with `cli`, `process`, and `security`. `tests/hermetic_cli.rs` exercises the
compiled binary using `CARGO_BIN_EXE_*` and a feature-gated fake-tool binary.
The real-account test remains opt-in in `tests/e2e_real_op.rs`.

## Process and data boundaries

`op`, the 1Password MCP server, `git`, `sh` on Unix, `gh`, `wrangler`,
`secretlint`, and the user-selected child are separate processes. Process
construction belongs in the owning adapter or in `process`; secret-bearing
capture and relay must use `security`. See [security.md](security.md) for the
trust model.

The item-list and legacy repository caches are implementation details. They may
store IDs, titles, vault metadata, normalized repository metadata, and
timestamps only. They never store item fields or resolved values.

## Platform and behavior

CI runs on Linux, macOS, and Windows. The hermetic harness installs the same
fake executable under platform-correct tool names and captures exact argv,
selected environment keys, stdin, streams, status, delays, and MCP responses.
Add platform-specific implementation only when the behavior cannot be shared,
and keep platform-specific tests behind `cfg`.

Behavioral changes must preserve the stable surface defined in
[compatibility.md](compatibility.md), especially output routing and exit codes.
