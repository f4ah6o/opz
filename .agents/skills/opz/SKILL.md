---
name: opz
description: Use the opz CLI to search 1Password items, inspect valid env labels, generate env files, create items from local files, store GitHub repository secrets, and run commands with secrets injected as environment variables.
---

# opz

Use this skill when you need to work with 1Password-backed secrets through the `opz` CLI. `opz` reads item metadata from `op`, builds `op://<vault_id>/<item_id>/<field>` references for valid env labels, and can resolve those references while running another command.

## Prerequisites

- 1Password CLI (`op`) is installed and authenticated.
- The relevant vault and item names are known, or can be discovered with `opz find`.

## Global Options

- `--vault <NAME>` limits item lookup to a specific 1Password vault.
- `--env-file <ENV>` writes generated env references to a file for `run` and `gen`.

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

### `gen`

Generate `op://...` environment variable references without running a command. Stdout is sectioned by item; file output is a merged key list.

```bash
opz gen [OPTIONS] <ITEM>...
opz gen --env-file .env.local <ITEM>...
```

### `create`

Create a 1Password item from `.env` or another private config file. Exact `.env` input creates an `API_CREDENTIAL` titled with `<ITEM>`. Other files create `SECURE_NOTE` items titled from parseable git remotes such as `org/repo`.

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

Store valid item fields as GitHub repository secrets.

```bash
opz github-secret [OPTIONS] <ITEM>...
opz github-secret --repo owner/repo <ITEM>...
opz github-secret --dry-run <ITEM>...
```

### `skills`

Print this bundled Agent Skills `SKILL.md` to stdout.

```bash
opz skills
```

## Behavior Notes

- When multiple items define the same env key, later items win.
- `github-secret` also uses later-item-wins and passes values to `gh secret set` through stdin.
- `github-secret` rejects names starting with `GITHUB_`.
- `gen` stdout uses `op://<vault_id>/<item_id>/<field>` references, not resolved secret values.
- When `--env-file` points to an existing file, `opz` appends new keys and overwrites duplicate keys while preserving unrelated lines.
- `show` only prints labels that are valid shell environment variable names.
- Item lists are cached for 60 seconds. Creating items invalidates that cache best-effort.
- OTLP tracing is disabled unless `OTEL_EXPORTER_OTLP_ENDPOINT` is set.

## Suggested Workflow

1. Discover candidate items with `opz find`.
2. Inspect available labels with `opz show`.
3. Use `opz gen --env-file ...` when another tool needs `op://` references in a file.
4. Use `opz github-secret --dry-run ...` before writing GitHub repository secrets.
5. Use `opz run ... -- <COMMAND>` when you want to execute a command with injected secrets directly.
