---
name: opz
description: Use the opz CLI to search 1Password items, inspect valid env labels, generate env files, create items from local files, and run commands with secrets injected as environment variables.
---

# opz

Use this skill when you need to work with 1Password-backed secrets through the `opz` CLI.

## Prerequisites

- 1Password CLI (`op`) is installed and authenticated.
- The relevant vault and item names are known, or can be discovered with `opz find`.

## Global Options

- `--vault <NAME>` limits item lookup to a specific 1Password vault.
- `--env-file <ENV>` writes generated env references to a file instead of stdout-only workflows.

## Commands

### `find`

Search item titles by keyword.

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

Generate `op://...` environment variable references without running a command.

```bash
opz gen [OPTIONS] <ITEM>...
opz gen --env-file .env.local <ITEM>...
```

### `create`

Create a 1Password item from `.env` or another private config file.

```bash
opz create <ITEM> [ENV]
```

### `run`

Run a command with secrets from one or more items injected as environment variables.

```bash
opz run [OPTIONS] <ITEM>... -- <COMMAND>...
opz [OPTIONS] <ITEM>... -- <COMMAND>...
```

### `skills`

Print this bundled Agent Skills `SKILL.md` to stdout.

```bash
opz skills
```

## Behavior Notes

- When multiple items define the same env key, later items win.
- `gen` stdout uses `op://vault/item/key` references, not resolved secret values.
- When `--env-file` points to an existing file, `opz` appends new keys and overwrites duplicate keys while preserving unrelated lines.
- `show` only prints labels that are valid shell environment variable names.

## Suggested Workflow

1. Discover candidate items with `opz find`.
2. Inspect available labels with `opz show`.
3. Use `opz gen --env-file ...` when another tool needs `op://` references in a file.
4. Use `opz run ... -- <COMMAND>` when you want to execute a command with injected secrets directly.
