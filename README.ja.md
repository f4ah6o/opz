# opz

`opz` は 1Password CLI の小さなラッパーです。アイテムを探し、有効なフィールド名を環境変数に変換し、その secret を入れた状態でコマンドを実行できます。

## 機能

* タイトルのキーワードで 1Password アイテムを検索します。
* `doctor` で `op` の認証状態、任意の依存 CLI、平文の `.env` 系 credential ファイルをチェックします。
* shell の環境変数名として使えるフィールドラベルを表示します。
* 1 つ以上の 1Password アイテムから secret を取得してコマンドを実行します。repository metadata による item 自動検出にも対応します。
* `op://...` 参照を含む env ファイルを生成し、既存ファイルの無関係な行は残します。
* 既存 script を、明示 item や `.env` から repository metadata ベースの実行へ migrate できます。
* private 設定ファイルを Secure Note として保存します。
* 有効なアイテムフィールドを GitHub repository secrets に保存し、item 側の repository metadata があれば誤配を防ぎます。
* 有効なアイテムフィールドを Wrangler 経由で Cloudflare Worker secrets に保存します。
* bundled `opz` Agent Skill を出力します。
* アイテムリストを 60 秒キャッシュし、完全一致がない場合はタイトルの部分一致で探します。

## インストール

```bash
cargo install opz
```

## Trusted publishing

このリポジトリは [crates.io trusted publishing](https://crates.io/docs/trusted-publishing) に対応しています。
`v2026.5.1` のようなタグを作成してプッシュすると、`Publish to crates.io` ワークフローがトリガーされ、OIDC 経由で短期間有効なトークンを取得し、`cargo publish --locked` を実行します。
ワークフローがトークンをリクエストできるようにするには、crates.io UI で `opz` クレートの trusted publishing を有効にします。リンク先 repository は `f4ah6o/opz` です。

## 使い方

### アイテム検索

タイトルのキーワードで 1Password アイテムを検索します:

```bash
opz find <query>
```

例:
```bash
opz find github
# 出力: <item-id>    <vault-name>    github-token
```

### Doctor

1Password CLI の状態と外部コマンド依存をチェックします:

```bash
opz doctor
```

`doctor` は必須の `op` チェックが失敗した場合だけ非 0 で終了します。`gh`、`wrangler`、`git`、`sh`、`secretlint` など任意ツールの欠落は警告として表示します。平文の `.env` 系 credential ファイルもチェックし、`secretlint` が使える場合はそのファイルに対して実行します。

### アイテムラベル表示

環境変数名として使えるフィールドラベルを表示します:

```bash
opz show [OPTIONS] [--with-item] <ITEM>...
```

オプション:
* `--vault <NAME>` - Vault 名（省略時はすべての Vault を検索）
* `--with-item` - アイテムごとの見出しを表示

例:
```bash
# ラベル名のみ（1行1ラベル）
opz show foo bar

# アイテム見出し付きで表示
opz show --with-item foo bar
```

### Agent Skill を出力

`opz` 用の bundled Agent Skills `SKILL.md` を出力します:

```bash
opz skills
```

これにより、他の agent や tool は Agent Skills 標準形式で `opz` の利用コンテキストをそのまま読み込めます。

### Secret 付きでコマンド実行

1 つ以上の 1Password アイテムから secret を取得してコマンドを実行します:

```bash
opz run [OPTIONS] [--env-file <ENV>] [<ITEM>...] -- <COMMAND>...
opz [OPTIONS] [--env-file <ENV>] [<ITEM>...] -- <COMMAND>...
```

オプション:
* `--vault <NAME>` - Vault 名（省略時はすべての Vault を検索）
* `--env-file <ENV>` - 出力 env ファイルパス。省略時はファイルを書きません。

引数:
* `<ITEM>...` - secret を取得する任意のアイテムタイトル。省略時は、現在の git remote と一致する `github_repositories` metadata を持つ item を自動検出します。

`--env-file` を指定すると、env ファイルはコマンド終了後も残ります。既存ファイルはマージされ、無関係な行は残り、重複キーは上書きされ、新しいキーは末尾に追加されます。複数アイテムで同じキーがある場合は後勝ちです（`opz run foo bar ...` では `bar` の値が優先）。

例:
```bash
# .env ファイルを生成せずに1アイテムで実行
opz run example-item -- your-command

# migrate 後の推奨: git remote metadata から item を自動検出
opz run -- your-command

# 複数アイテムで実行（重複キーは後勝ち）
opz run foo bar -- your-command

# 他の tool が op:// 参照ファイルを必要とするときだけ env ファイルを生成
opz run --env-file .env foo bar -- your-command

# 短縮形でも複数アイテム対応
opz --env-file .env.local foo bar -- your-command

# shell ではなく opz に展開させるため、変数を quote する
opz run my-service -- curl -H 'Authorization: Bearer $API_TOKEN' https://api.example.test

# Vault を指定
opz run --vault Private foo bar -- your-command
```

### Env ファイル生成

コマンドを実行せず、`op://...` の env 参照を生成します:

```bash
opz gen [OPTIONS] [--env-file <ENV>] <ITEM>...
```

例:
```bash
# セクション付きで標準出力
opz gen foo bar

# .env ファイルを生成
opz gen --env-file .env foo bar

# カスタムパスに生成
opz gen --env-file .env.production foo bar

# Vault を指定
opz --vault Private gen foo bar
```

標準出力では `# --- item: <title> ---` のようなコメント見出しを付けます。ファイル出力では、セクションコメントなしのマージ済みキー一覧を書きます。

### Script と `.env` の migrate

`justfile` / `Justfile` の recipe と `package.json` の scripts を、repository metadata と item 自動検出へ移行します:

```bash
opz migrate [OPTIONS]
```

オプション:
* `--dry-run` - 1Password item やファイルを編集せず、変更内容だけを表示
* `--new` - `.env` から新しい `API_CREDENTIAL` item を作成してから `.env` ベースの script を書き換えます。item 名は先頭の git remote repository 名です。
* `--vault <NAME>` - Vault 名（省略時はすべての Vault を検索）

挙動:
* `opz run <ITEM> -- <COMMAND>` は、現在の git remote を `<ITEM>` の metadata に記録してから `opz run -- <COMMAND>` に書き換えます。
* `opz <ITEM> -- <COMMAND>` も同様に `opz -- <COMMAND>` へ書き換えます。
* `op item get <ITEM>` は metadata 登録の手がかりとして使いますが、`opz run` と等価ではないため書き換えません。
* `.env` ベースの script は `--new` 指定時だけ書き換えます。指定がない場合は報告してスキップします。

例:
```bash
# 変更内容だけ確認
opz migrate --dry-run

# script を書き換え、item metadata を更新
opz migrate

# .env から新しい item を作成して migrate
opz migrate --new
```

### Private 設定ファイルを Secure Note として保存

private 設定ファイルを git remote 由来のタイトルで Secure Note item として保存します:

```bash
opz note <FILE>
```

挙動:
* 本文は ```` ```<file name>\n<content>\n``` ```` の形で保存します。
* タイトルには git remote URL から抽出した `org/repo` を使います。
* remote が複数ある場合は remote ごとに作成し、同名時は `-2`, `-3` のように番号を付けます。
* 解釈できる git remote がない場合はエラー終了します。

例:
```bash
opz note app.conf
opz --vault Private note app.conf
```

### 既存 item に GitHub Repository Metadata を追加

既存の 1Password item に `github_repositories` metadata を追加または更新します:

```bash
opz github-repo [OPTIONS] <ITEM>...
```

オプション:
* `--repo <OWNER/REPO>` - 記録する repository。複数回指定できます。省略時は現在の repository の git remote から解釈できる値を使います。
* `--dry-run` - item を編集せず、更新内容だけを表示
* `--vault <NAME>` - Vault 名（省略時はすべての Vault を検索）

例:
```bash
# 現在の git remote を使った migration を事前確認
opz github-repo --dry-run my-service shared-secrets

# 現在の git remote repository metadata を追加
opz github-repo my-service shared-secrets

# 明示した repository を追加
opz github-repo --repo owner/repo --repo other/service my-service
```

既存の `github_repositories` は残し、指定した repository とマージします。

### GitHub Repository Secrets に保存

有効なアイテムフィールドを GitHub repository secrets に保存します:

```bash
opz github-secret [OPTIONS] <ITEM>...
```

オプション:
* `--repo <OWNER/REPO>` - 保存先 GitHub repository（省略時は現在の `gh` repository）
* `--dry-run` - 値を書き込まず、保存対象の secret 名だけを表示
* `--vault <NAME>` - Vault 名（省略時はすべての Vault を検索）

例:
```bash
# secret 名を事前確認
opz github-secret --dry-run my-service

# 現在の repository に保存
opz github-secret my-service

# repository を指定して保存
opz github-secret --repo owner/repo my-service shared-secrets
```

`github-secret` は `gen` や `run` と同じ有効フィールドラベルを使います。複数 item で同名がある場合は後勝ちです。Secret 値はメモリ上で解決し、`gh secret set` の stdin に渡します。値を表示したりコマンド引数に載せたりしません。GitHub が予約しているため、`GITHUB_` で始まる名前は拒否します。

選択した 1Password item に `github_repositories` フィールドがある場合、保存先 repository はその `owner/repo` 一覧のいずれかと一致する必要があります。一覧は改行またはカンマ区切りで複数指定できます。この metadata がない item は従来通り利用できますが、repository guard を適用できないため警告を表示します。

### Cloudflare Worker Secrets に保存

有効なアイテムフィールドを Wrangler 経由で Cloudflare Worker secrets に保存します:

```bash
opz cloudflare-secret [OPTIONS] <ITEM>...
```

オプション:
* `--name <WORKER>` - `wrangler secret bulk --name` に渡す Worker 名
* `--env <ENV>` - `wrangler secret bulk --env` に渡す Wrangler environment
* `--config <PATH>` - `wrangler secret bulk --config` に渡す Wrangler config path
* `--dry-run` - 値を書き込まず、保存対象の secret 名だけを表示
* `--vault <NAME>` - Vault 名（省略時はすべての Vault を検索）

例:
```bash
# secret 名を事前確認
opz cloudflare-secret --dry-run my-service

# 現在の Wrangler project config を使って保存
opz cloudflare-secret my-service

# Worker と environment を指定して保存
opz cloudflare-secret --name worker-app --env production my-service shared-secrets
```

`cloudflare-secret` は `gen` や `run` と同じ有効フィールドラベルを使います。複数 item で同名がある場合は後勝ちです。Secret 値はメモリ上で解決し、JSON として `wrangler secret bulk` の stdin に渡します。値を表示したりコマンド引数に載せたりしません。

## 仕組み

1. 1Password からアイテムリストを取得し、そのメタデータを 60 秒キャッシュします。
2. タイトルで一致するアイテムを 1 件探します。まず完全一致、見つからなければタイトルの部分一致を使います。
3. 一致したアイテムを取得し、有効な env ラベルを持つフィールドから `op://<vault_id>/<item_id>/<field>` 参照を作ります。
4. env ファイル指定があれば、既存ファイルとマージして書き込みます。推奨は file-free な `opz run` で、env ファイルは `op://` 参照を必要とする tool 向けです。
5. `op run --env-file <temp> -- sh -c 'env -0'` で secret をまとめて解決し、失敗した場合は参照ごとに `op read` へフォールバックします。
6. 解決済みの値を環境変数に入れてコマンドを実行します。コマンド引数内の `$VAR` と `${VAR}` は、選択したアイテムから解決できた変数だけ展開します。

`gen` は参照を書いたところで終了します。`show` は secret 値を解決せず、有効なラベルだけを表示します。

## `op` コマンドの利用

セキュリティの透明性のため、`opz` が `op` CLI をどのように利用するかを示します:

```mermaid
sequenceDiagram
    participant opz
    participant op as op CLI

    Note over opz: ユーザー実行: opz example-item -- claude "hello"

    opz->>op: op item list --format json
    op-->>opz: [{id, title, vault}, ...]
    Note over opz: "example-item" にマッチ → アイテム ID を取得

    opz->>op: op item get <id> --format json
    op-->>opz: {fields: [{label, value}, ...]}
    Note over opz: secret 値を解決<br/>（環境変数として注入）

    Note over opz: オプション: .env ファイルを指定時は書き込み

    opz->>op: sh -c "claude \"hello\""
    Note over opz: secret を含む環境変数で実行
    op-->>opz: 終了ステータス
```

セキュリティ: `opz` は secret へのアクセスと認証を `op` CLI に任せます。60 秒キャッシュするのはアイテムリストのメタデータだけで、secret 値は保存しません。

## Tracing（OpenTelemetry + Jaeger）

`opz` は OTLP trace 出力に対応していますが、デフォルトでは無効です。`OTEL_EXPORTER_OTLP_ENDPOINT` が未設定の場合は no-op として動作します。

### ローカル手順

```bash
just jaeger-up
just trace-run item=<your-item-title>
just trace-ui
```

### E2E trace を Jaeger で見る

`tests/e2e_real_op.rs` が生成する trace を確認したい場合:

```bash
just jaeger-up
just e2e-trace
just trace-ui
```

Jaeger の Search で service `opz-e2e` を選択してください。  
`just e2e-trace` は `OPZ_GIT_COMMIT=$(git rev-parse --short=12 HEAD)` を自動設定します。

### ref / version 単位で trace を比較する

比較したい各 commit / tag / version で trace を生成した後、次を実行します:

```bash
just trace-report <ref-or-version>
just trace-compare <base-ref-or-version> <head-ref-or-version>
```

`<ref-or-version>` には commit hash、git tag（例: `v2026.5.1`）、`service.version`（例: `2026.5.1`）を指定できます。
どちらも markdown テーブル（duration と最長 child span）を標準出力します。

比較ノイズを減らすには、複数サンプル集計＋失敗トレース除外を使います:

```bash
just trace-report-samples <ref-or-version> samples=5 status=ok
just trace-compare-samples <base-ref-or-version> <head-ref-or-version> samples=5 status=ok
```

`samples` は operation ごとの最新 N 件を集計し、中央値/平均を出力します。
`status` は `all` / `ok` / `error` を指定できます。

Jaeger の Search で service `opz`（または `OTEL_SERVICE_NAME`）を選び、以下の span を確認できます:

* `cli.<command>`（root）
* `parse_args`
* `load_config`
* `load_inputs`
* `main_operation`
* `write_outputs`

### 環境変数

* `OTEL_EXPORTER_OTLP_ENDPOINT` - 設定時のみ OTLP export を有効化（例: `http://localhost:4317`）
* `OTEL_SERVICE_NAME` - service 名の任意上書き（デフォルト: `opz`）
* `OTEL_TRACES_SAMPLER` - sampler 設定（`always_on`, `traceidratio` など）
* `OTEL_TRACES_SAMPLER_ARG` - ratio sampler 用パラメータ
* `OPZ_TRACE_CAPTURE_ARGS` - `1` のときのみサニタイズ済み `cli.args` を属性記録（デフォルト: 無効）
* `OPZ_GIT_COMMIT` - trace の resource 属性 `git.commit` の任意上書き（デフォルト: `git rev-parse --short=12 HEAD`）

## 要件

* [1Password CLI](https://developer.1password.com/docs/cli/) (`op`) がインストールされ、認証されていること
* 任意: `github-secret` 用の GitHub CLI (`gh`)
* 任意: `cloudflare-secret` 用の Wrangler (`wrangler`)
* 任意: `migrate` と `note` 用の Git (`git`)

## E2Eテスト

実際の1Passwordを使うe2eテストは `tests/e2e_real_op.rs` にあります。

安全のため、`OPZ_E2E=1` を指定した場合にのみ実行されます:

```bash
OPZ_E2E=1 cargo test --test e2e_real_op -- --nocapture
```

または `just` で実行できます:

```bash
just e2e
```
