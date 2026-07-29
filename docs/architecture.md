# Architecture

`opz` is a CLI application. Its library target exists only to keep the binary
entrypoint small and is not a supported Rust API.

The dependency direction starts at `cli` and flows toward small domain and
process helpers:

- `cli` owns clap parsing and command dispatch.
- `op` owns 1Password item models, templates, and CLI adapter operations.
- `mcp` owns JSON-RPC stdio transport and Developer Environment operations.
- `environment` owns native `op run` Environment delegation.
- `resolver` owns item lookup, repository matching, and metadata-only caches.
- `envfile` owns env parsing, merging, rendering, and file output.
- `migration` owns package.json, justfile, and shell-script rewrites.
- `targets` owns GitHub and Cloudflare export policy.
- `doctor` owns diagnostic checks and rendering.
- `process` owns child execution and timeouts.
- `security` owns output masking and other secret-bearing policy helpers.
- `skill` and `instrumentation` own the bundled Agent Skill and telemetry seams.

Lower modules accept small domain inputs such as `ItemContext`; they do not
depend on clap command types. Cross-module visibility should remain
`pub(crate)` or narrower so the crate does not accidentally grow a supported
library API.

Behavioral changes must be covered by the hermetic CLI suite. In particular,
preserve documented commands and aliases, stdout/stderr routing, cache
semantics, external-process argv construction, and exit codes.
