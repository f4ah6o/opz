# 1Password Desktop SDK availability diagnostics and guidance

## Status

Open

## Objective

`opz` が 1Password Desktop SDK の利用可否を診断し、SDK integration が無効または利用不能な場合に、ユーザーが設定箇所まで迷わず到達できる導線を提供する。

## Problem

`opz 2026.9.0` は Desktop SDK を優先し、失敗時に official `op` CLI へ fallback する。しかし現状は以下の問題がある。

- `opz doctor` が Desktop SDK の利用可否を検査していない。
- Desktop SDK が無効でも通常コマンドは fallback して動くため、SDK fast path が使われていないことに気付きにくい。
- SDK integration を有効化する 1Password Desktop の設定箇所が CLI から案内されない。

## Proposed UX

### `opz doctor`

Desktop SDK を副作用なしで probe し、結果を明示する。

成功例:

```text
ok    1Password Desktop SDK: connected
```

利用不能例:

```text
warn  1Password Desktop SDK: unavailable; enable: Settings → Developer → Integrate with the 1Password SDKs → Integrate with other apps
```

`OPZ_ONEPASSWORD_SDK=off` の場合は設定不備と誤認させず、明示的に disabled と表示する。

```text
warn  1Password Desktop SDK: disabled by OPZ_ONEPASSWORD_SDK=off
```

### Normal command fallback

Desktop SDK の利用を試行して失敗し、`op` CLI fallback へ移行した場合、同一 `opz` invocation 内で一度だけ stderr に診断ヒントを出す。

```text
warn: 1Password Desktop SDK unavailable; using op CLI fallback. enable: Settings → Developer → Integrate with the 1Password SDKs → Integrate with other apps
```

同じ invocation 中に複数の SDK stage が失敗しても繰り返し表示しない。

## Constraints

- secret value、item payload、raw SDK error を diagnostic output に含めない。
- probe は read-only とし、item/vault の作成・更新・削除を行わない。
- SDK probe が失敗しても、既存の item-backed CLI fallback を壊さない。
- `OPZ_ONEPASSWORD_SDK=off` は既存どおり SDK を完全に無効化する。
- stdout を script-oriented output として利用している既存コマンドでは、fallback guidance を stderr に出す。
- `doctor` の SDK failure は optional warning とし、`op` CLI fallback が利用可能なら doctor 全体を failure にしない。

## Implementation plan

1. Desktop SDK availability probe を `resolver` / `sdk_bridge` の既存 read-only `vaults_list` 経路を再利用して実装する。
2. `doctor` に `1Password Desktop SDK` check を追加する。
3. SDK failure を process-scoped に一度だけ通知する helper を追加する。
4. SDK read / resolve path の fallback 箇所から helper を呼ぶ。
5. secret-bearing upstream details を表示しない regression test を追加する。
6. `README.md`、`README.ja.md`、`.agents/skills/opz/SKILL.md` の診断説明を更新する。

## Acceptance criteria

- SDK integration 有効時、`opz doctor` に `ok    1Password Desktop SDK: connected` が出る。
- SDK integration 無効または SDK authorization / IPC 利用不能時、`opz doctor` に指定の enable 導線が出る。
- `OPZ_ONEPASSWORD_SDK=off` 時、設定案内ではなく disabled 理由が出る。
- 通常コマンドの SDK→CLI fallback 時、案内は stderr に最大1回だけ出る。
- SDK failure details や secret-bearing data が診断に漏れない。
- SDK unavailable 環境でも既存 CLI fallback の機能・exit semantics を維持する。
- `just check` が通る。
