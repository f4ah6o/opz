---
name: jaeger-trace-compare
description: Extract and compare Jaeger traces for opz/e2e by git ref (commit/tag) or service.version, producing markdown tables via just recipes. Use when you need performance before/after evidence tied to releases.
---

# Jaeger Trace Compare

Use this skill when you need reproducible performance comparison from Jaeger traces between two code versions.

## Prerequisites

- Jaeger is running (`just jaeger-up`)
- Traces are generated with `OTEL_EXPORTER_OTLP_ENDPOINT` set
- `git.commit` resource attribute is present (added by opz telemetry)

## Standard workflow

1. Generate traces on each target commit:

```bash
just e2e-trace
```

2. Collect per-ref report:

```bash
just trace-report <ref-or-version>
```

3. Compare two refs:

```bash
just trace-compare <base-ref-or-version> <head-ref-or-version>
```

4. For stable numbers, use multi-sample aggregation:

```bash
just trace-report-samples <ref-or-version> samples=5 status=ok
just trace-compare-samples <base-ref-or-version> <head-ref-or-version> samples=5 status=ok
```

## Output

`trace-report` and `trace-compare` print markdown tables that can be pasted into PR comments and release notes.

## Troubleshooting

If no traces are found, increase the fetch limit with `limit=1000`.
If a ref is not matched, pass a longer commit prefix or an explicit tag.
If the service name differs, pass `service=<name>`.
If failures skew the numbers, set `status=ok`.

## Reference

Jaeger API details live in `references/jaeger-api.md`.
