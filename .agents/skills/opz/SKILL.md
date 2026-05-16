---
name: opz
description: Use the opz CLI to search 1Password items, inspect valid env labels, diagnose op and dependency status, generate env files, create items from local files, store GitHub repository secrets, store Cloudflare Worker secrets, and run commands with secrets injected as environment variables.
---

# opz

Use this skill when you need to work with 1Password-backed secrets through the `opz` CLI. `opz` reads item metadata from `op`, builds `op://<vault_id>/<item_id>/<field>` references for valid env labels, and can resolve those references while running another command.

## Prerequisites

- 1Password CLI (`op`) is installed and authenticated.
- The relevant vault and item names are known, or can be discovered with `opz find`.

## Global Options

- `--vault <NAME>` limits item lookup to a specific 1Password vault.
- `--env-file <ENV>` writes generated `op://` references to a file for `run` and `gen`; prefer file-free `run` unless another tool requires an env file.

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

### `create`

Create a 1Password item from `.env` or another private config file. Exact `.env` input creates an `API_CREDENTIAL` titled with `<ITEM>` and records parseable git remotes in `github_repositories`. Other files create `SECURE_NOTE` items titled from parseable git remotes such as `org/repo`.

```bash
opz create <ITEM> [ENV]
```

### `run`

Run a command with secrets from one or more items injected as environment variables. `$VAR` and `${VAR}` in command arguments are expanded only when that variable was resolved from the selected items.

```bash
opz run [OPTIONS] <ITEM>... -- <COMMAND>...
opz [OPTIONS] <ITEM>... -- <COMMAND>...
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

## Behavior Notes

- When multiple items define the same env key, later items win.
- `doctor` checks `op` as required and reports `gh`, `wrangler`, `git`, `sh`, `secretlint`, and plaintext `.env`-style files as optional warnings.
- `github-secret` also uses later-item-wins and passes values to `gh secret set` through stdin.
- `github-secret` rejects names starting with `GITHUB_` and blocks writes when item `github_repositories` metadata does not include the target repo.
- `github-repo` migrates existing items by merging repository metadata into `github_repositories`.
- `github_repositories` is metadata, not an env label or deployable secret.
- `cloudflare-secret` also uses later-item-wins and passes a JSON payload to `wrangler secret bulk` through stdin.
- `cloudflare-secret` supports `--name`, `--env`, and `--config` for Wrangler target selection.
- `gen` stdout uses `op://<vault_id>/<item_id>/<field>` references, not resolved secret values.
- When `--env-file` points to an existing file, `opz` appends new keys and overwrites duplicate keys while preserving unrelated lines.
- `show` only prints labels that are valid shell environment variable names.
- Item lists are cached for 60 seconds. Creating items invalidates that cache best-effort.
- OTLP tracing is disabled unless `OTEL_EXPORTER_OTLP_ENDPOINT` is set.

## Suggested Workflow

1. Discover candidate items with `opz find`.
2. Run `opz doctor` if `op` authentication or a dependency CLI looks suspicious.
3. Inspect available labels with `opz show`.
4. Use `opz run ... -- <COMMAND>` for the normal file-free workflow.
5. Use `opz gen --env-file ...` only when another tool needs `op://` references in a file.
6. Use `opz github-repo --dry-run ...` to preview repository metadata migration for older items.
7. Use `opz github-secret --dry-run ...` before writing GitHub repository secrets.
8. Use `opz cloudflare-secret --dry-run ...` before writing Cloudflare Worker secrets.
