# Item-title environment namespacing

`opz` can prefix generated environment variables with the source 1Password item title. This is opt-in and is intended for commands that load multiple items containing common field labels such as `API_TOKEN`, `BASE_URL`, or `USERNAME`.

## Usage

Run a command with independent credentials from multiple items:

````bash
opz run --namespace item-title service_12 service_18 -- pnpm dev
````

The child process receives keys such as:

````text
SERVICE_12__API_TOKEN
SERVICE_18__API_TOKEN
````

The top-level shorthand is equivalent:

````bash
opz --namespace item-title service_12 service_18 -- pnpm dev
````

Generate namespaced `op://` references without running a command:

````bash
opz gen --namespace item-title service_12 service_18
opz gen --namespace item-title --env-file .env.local service_12 service_18
````

Inspect the generated names without resolving values:

````bash
opz show --namespace item-title service_12 service_18
opz show --namespace item-title --with-item service_12 service_18
````

## Normalization

For each item title, `opz`:

1. Converts ASCII letters to uppercase.
2. Replaces each run of characters outside `[A-Z0-9_]` with `_`.
3. Trims leading and trailing underscores.
4. Joins the normalized title and the existing valid field label with `__`.

Examples:

| Item title | Field label | Generated key |
|---|---|---|
| `service_12` | `API_TOKEN` | `SERVICE_12__API_TOKEN` |
| `team/service-a` | `BASE_URL` | `TEAM_SERVICE_A__BASE_URL` |
| `Production API` | `USERNAME` | `PRODUCTION_API__USERNAME` |

An item title that becomes empty after normalization is rejected. If two item-title and field-label pairs generate the same key, `opz` reports the generated key and conflicting item titles before resolving secrets or starting the child process.

Without `--namespace item-title`, existing later-item-wins merge behavior is unchanged.
