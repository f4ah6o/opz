# Secret-handling security policy

## Trust boundaries

`opz` trusts the local user, the selected 1Password account, and the command
the user explicitly asks it to run. The `op`, 1Password MCP, `gh`, and
`wrangler` executables are separate processes and must be obtained from trusted
sources. A child command receives resolved secrets in its environment and is
therefore an explicit trust boundary: it can read, transmit, or print them.

Process listings, terminal output, tracing backends, error reports, repository
files, caches, and other users on the same machine are not secret channels.

## Required invariants

- Resolved secret values MUST NOT be placed in `opz`, `op`, `gh`, `wrangler`,
  shell, or child argv. `opz` passes user command arguments unchanged.
- Resolved values MUST be delivered only through the child environment or a
  documented exporter stdin payload. GitHub receives one value on
  `gh secret set` stdin; Cloudflare receives JSON on `wrangler secret bulk`
  stdin.
- `SecretValue` MUST redact `Debug` and `Display`. Known secret values MUST pass
  through the shared longest-match `Redactor` before captured external-tool
  stdout or stderr is relayed or used as error context.
- Instrumentation MUST record operation names, counts, paths, and statuses
  only. It MUST NOT record resolved values, secret-bearing payloads, command
  environments, MCP response bodies, or secret-bearing argv.
- Parse failures and error context MUST NOT include raw 1Password or MCP
  response bodies. MCP errors expose only a numeric error code when available.
- Dry-run modes MUST NOT resolve secret values. They may read item metadata and
  print destination names.
- Metadata caches MUST contain only item IDs, titles, vault metadata, and
  normalized repository metadata. Cache formats are internal, but values and
  item fields MUST NOT be added.
- Temporary secret-bearing files MUST be uniquely created, use mode `0600` on
  Unix, and be removed on every normal success or error path.
- Persistent env targets MUST reject symlinks and non-regular files. Rewrites
  MUST use a unique same-directory replacement. New files use mode `0600` on
  Unix; an existing regular file keeps its permissions.
- Persistent files written by `gen` or `run --env-file` contain `op://`
  references, not resolved values. Existing unrelated comments and lines are
  intentionally preserved and remain the user's responsibility.
- 1Password Developer Environment operations MUST use MCP only for account and
  Environment management, variable names, and mount metadata. `opz` MUST NOT
  request or print Environment variable values. Placeholder creation may send
  only validated variable names, an empty value, and `concealed=true`; real
  values must be entered in 1Password.
- Raw MCP stderr MUST NOT be relayed. Connection diagnostics may map recognized
  permission or feature-disabled errors to fixed, non-secret hints.

## Safe usage

Prefer file-free execution:

```sh
opz run my-service -- your-command
```

Read a value inside the trusted child from its environment or stdin rather than
embedding it in an argument:

```sh
opz run my-service -- sh -c 'printf "%s" "$API_TOKEN" | trusted-consumer'
```

Shell expansion in that example is performed by the explicitly trusted child
shell. `opz` does not substitute `$VAR` or `${VAR}` in argv.

Report suspected leaks without including the secret value. Rotate any exposed
credential before collecting logs or opening an issue.
