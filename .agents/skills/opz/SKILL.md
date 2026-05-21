---
name: opz
description: Use the opz CLI to search 1Password items, inspect valid env labels, diagnose op and dependency status, generate env files, migrate scripts to repository item titles and metadata, store private files as notes, store GitHub repository secrets, store Cloudflare Worker secrets, and run commands with item-backed or 1Password Environment-backed secret injection.
---

# opz

Use this skill when you need to work with 1Password-backed secrets through the `opz` CLI. `opz` reads item metadata from `op`, builds `op://<vault_id>/<item_id>/<field>` references for valid env labels, and can resolve those references while running another command. When using 1Password Environments, `opz run --environment <ENV> -- <COMMAND>` delegates to native `op run` so `opz` does not read Environment secret values.

## Prerequisites

- 1Password CLI (`op`) is installed and authenticated.
- The relevant vault and item names are known, can be discovered with `opz find`, or can be auto-detected from item titles that match the current git remote repository name.

## Global Options

- `--vault <NAME>` limits item lookup to a specific 1Password vault.
- `--env-file <ENV>` writes generated `op://` references to a file for `run` and `gen`; prefer file-free `run` unless another tool requires an env file.
- `--environment <ENV>` / `--environments <ENV>` uses native 1Password Environments injection through `op run`; do not combine it with item arguments or `--env-file`.

## Commands

### `find`

Search item titles by keyword. Output rows are item id, vault name, and title.

```bash
opz find <query>
```

### `show`

List valid environment variable labels from one or more 1Password items.

```bash
opz show [OPTIONS] <ITEM>...
opz show --with-item <ITEM>...
```

### `doctor`

Check `op` authentication, external command dependencies, and plaintext `.env`-style credential files. Required `op` failures exit non-zero; missing optional tools and credential-file findings are warnings.

```bash
opz doctor
```

### `gen`

Generate `op://...` environment variable references without running a command. Stdout is sectioned by item; file output is a merged key list.

```bash
opz gen [OPTIONS] <ITEM>...
opz gen --env-file .env.local <ITEM>...
```

### `migrate`

Migrate `justfile`/`Justfile` recipes and `package.json` scripts from explicit item names or `.env` usage to repository item titles and metadata. `--new` creates an `API_CREDENTIAL` item from `.env` first. `--restore` restores explicit item arguments for scripts that were previously migrated to itemless auto-detection.

```bash
opz migrate [OPTIONS]
opz migrate --dry-run
opz migrate --new
opz migrate --restore
```

### `note`

Store a private config file as Secure Note item(s) titled from parseable git remotes such as `org/repo`.

```bash
opz note <FILE>
```

### `run`

Run a command with secrets from one or more items injected as environment variables. `$VAR` and `${VAR}` in command arguments are expanded only when that variable was resolved from the selected items.
When no item is passed, `run` auto-detects exactly one item whose title matches the current git remote repository name such as `owner/repo`.

```bash
opz run [OPTIONS] [<ITEM>...] -- <COMMAND>...
opz [OPTIONS] [<ITEM>...] -- <COMMAND>...
opz run --environment <ENV> -- <COMMAND>...
opz --environment <ENV> -- <COMMAND>...
```

### `github-secret`

Store valid item fields as GitHub repository secrets. If an item has `github_repositories` metadata, the target repository must match before secrets are resolved or written.

```bash
opz github-secret [OPTIONS] <ITEM>...
opz github-secret --repo owner/repo <ITEM>...
opz github-secret --dry-run <ITEM>...
```

### `github-repo`

Add or update `github_repositories` metadata on existing 1Password items. `--repo` can be repeated; if omitted, parseable git remotes from the current repository are used.

```bash
opz github-repo [OPTIONS] <ITEM>...
opz github-repo --repo owner/repo --repo other/service <ITEM>...
opz github-repo --dry-run <ITEM>...
```

### `cloudflare-secret`

Store valid item fields as Cloudflare Worker secrets through Wrangler.

```bash
opz cloudflare-secret [OPTIONS] <ITEM>...
opz cloudflare-secret --name worker-app --env production <ITEM>...
opz cloudflare-secret --dry-run <ITEM>...
```

### `skills`

Print this bundled Agent Skills `SKILL.md` to stdout.

```bash
opz skills
```

### removed `create`

`opz create` is hidden and only returns a migration error. Use `opz migrate --new` for `.env` imports and `opz note <FILE>` for non-`.env` private files.

## Behavior Notes

- When multiple items define the same env key, later items win.
- `doctor` checks `op` as required and reports `gh`, `wrangler`, `git`, `sh`, `secretlint`, and plaintext `.env`-style files as optional warnings.
- `github-secret` also uses later-item-wins and passes values to `gh secret set` through stdin.
- `github-secret` rejects names starting with `GITHUB_` and blocks writes when item `github_repositories` metadata does not include the target repo.
- `github-repo` migrates existing items by merging repository metadata into `github_repositories`.
- `migrate` keeps explicit `opz run <ITEM> --` usage by default, updates item metadata, and when exactly one item and one repository are present, renames the item title and matching Just item variable to `owner/repo`; use `--dry-run` to preview. Dry runs do not fetch full item details; they report the repository metadata that would be ensured.
- `migrate --restore` rewrites itemless `opz run --` usage back to explicit item arguments where it can infer the item from a Just recipe parameter or current repository title.
- `migrate` treats `op item get <ITEM>` as a metadata signal but does not rewrite it.
- `migrate` patches matching `package.json` script strings without reserializing the whole file.
- `run` auto-detects an item when no item is passed and exactly one item title matches the current git remote repository. Legacy `github_repositories` scanning is opt-in with `OPZ_AUTODETECT_LEGACY_SCAN=1`.
- Environment-backed `run` is delegated to `op run` and does not resolve secret values in `opz`. Use the 1Password MCP server for agent-side Environment creation, variable-name inspection, and local `.env` mounting.
- `github_repositories` is metadata, not an env label or deployable secret.
- `cloudflare-secret` also uses later-item-wins and passes a JSON payload to `wrangler secret bulk` through stdin.
- `cloudflare-secret` supports `--name`, `--env`, and `--config` for Wrangler target selection.
- `gen` stdout uses `op://<vault_id>/<item_id>/<field>` references, not resolved secret values.
- When `--env-file` points to an existing file, `opz` appends new keys and overwrites duplicate keys while preserving unrelated lines.
- `show` only prints labels that are valid shell environment variable names.
- Explicit item titles are resolved with direct `op item get <title>` first; item list caching is used for fuzzy title fallback and legacy migration paths.
- Item lists and the legacy auto-detect repository index are cached for 60 seconds. Creating or editing items invalidates those caches best-effort.
- Secret-resolution `op` calls time out after 30 seconds by default. Set `OPZ_OP_TIMEOUT_SECONDS=<seconds>` to allow slower 1Password CLI operations. Batch secret-resolution timeouts stop immediately instead of retrying once per secret.
- OTLP tracing is disabled unless `OTEL_EXPORTER_OTLP_ENDPOINT` is set.

## Suggested Workflow

1. Discover candidate items with `opz find`.
2. Run `opz doctor` if `op` authentication or a dependency CLI looks suspicious.
3. Inspect available labels with `opz show`.
4. Use `opz run ... -- <COMMAND>` for the normal file-free workflow.
5. Use `opz run --environment <ENV> -- <COMMAND>` when the project is managed through 1Password Environments and the local `op` CLI supports native Environment injection.
6. Use `opz gen --env-file ...` only when another tool needs `op://` references in a file.
7. Use `opz migrate --dry-run` to preview script migration, then `opz migrate`, `opz migrate --new`, or `opz migrate --restore`.
8. Use `opz github-repo --dry-run ...` to manually add repository metadata for older items.
9. Use `opz github-secret --dry-run ...` before writing GitHub repository secrets.
10. Use `opz cloudflare-secret --dry-run ...` before writing Cloudflare Worker secrets.
