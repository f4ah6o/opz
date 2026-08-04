# Compatibility policy

This policy describes the supported command-line surface. The Rust library
target is not a supported API.

## Stable surface

The following are stable once documented in CLI help, `README.md`, or
`README.ja.md`:

- command and subcommand names;
- flags, positional arguments, defaults, and documented aliases;
- documented environment variables;
- stdout intended for scripts, including row/line structure and generated env
  syntax;
- whether a diagnostic is written to stdout or stderr;
- exit-code semantics;
- persistent env-file merge behavior and the metadata-only property of caches.

The bundled `.agents/skills/opz/SKILL.md` describes the same supported CLI and
must stay synchronized with both READMEs and clap help. The `plugin` command,
`NAME[@VERSION]` selector, lifecycle behavior, and item pin field names are part
of that stable surface. Individual registry releases remain immutable data and
are versioned independently in `opz-plugin`.

The following environment variables are currently documented user-facing
interfaces:

- `OPZ_1PASSWORD_MCP_COMMAND`;
- `OPZ_MCP_TIMEOUT_SECONDS`;
- `OPZ_OP_TIMEOUT_SECONDS`;
- `OPZ_AUTODETECT_LEGACY_SCAN`;
- `OPZ_PLUGIN_REGISTRY_DIR`.

Test-only controls such as `OPZ_E2E` and fake-tool protocol variables are not
user-facing interfaces. Undocumented implementation controls are internal.

## Output and exit behavior

Machine-oriented stdout must not gain headings, decoration, or diagnostics
without an explicit compatibility decision. Human-readable wording may be
clarified, but its stream and any tested structure remain stable. `doctor`
diagnostics intentionally use stdout.

Clap retains its own exit codes: successful help/version display exits 0 and
argument parsing errors use Clap's failure code. Runtime failures—including
required `doctor` failures, external-tool failures, timeouts, and a failing
child command—exit 1. When an external process supplied a different status,
the diagnostic retains that source status without adopting it as the `opz`
process exit code.

## Internal formats

Cache encoding, paths, and expiry details are internal and may change without a
deprecation period. The invariant that caches contain metadata only is stable
and security-sensitive.

The `test-support` feature, fake-tool binary, JSON scenario format, and hidden
library entrypoint are repository testing/organization interfaces. They are not
supported for downstream use.

## Breaking changes

A breaking change requires:

1. an issue that identifies the affected stable surface and migration;
2. release notes;
3. a deprecation period when a safe compatibility shim is possible.

Removal must update clap help, both READMEs, the bundled skill, compatibility
tests, and migration guidance in the same change. Urgent security hardening may
skip deprecation when retaining behavior would expose secrets; document the
reason and migration prominently in the issue and release notes.
