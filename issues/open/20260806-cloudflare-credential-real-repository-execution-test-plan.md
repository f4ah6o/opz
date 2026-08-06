# Cloudflare credential management: real-repository execution test plan

## Status

Open

## Objective

`opz cloudflare-credential`を、`gh repo clone`で取得した実リポジトリの作業ディレクトリ上で実行し、入力、1Password item作成・更新、redact、dry-run、secret reference出力、cleanupを一連で検証する。

対象リポジトリ:

- `f4ah6o/shuttle-rs`
- `f4ah6o/local-mcp`

テストで作成したclone、fixture、ログ、1Password item、temporary directoryは、成功・失敗を問わずすべて削除する。

## Scope

検証対象:

- 入力元: `--stdin`、`--file <JSON>`、`-- <COMMAND>...`
- preset: `api-token`、`worker-secret`、`api-response`
- item操作: `create`、`update`、`upsert`
- `--vault`、`--item`、`--section`、`--field`
- `--dry-run`
- APIレスポンスの既定redact
- `api-response`での明示的な`--raw`
- 成功時にsecret値ではなく`op://` referenceのみ出力すること
- secret値をargv、設定ファイル、通常ログへ残さないこと

対象外:

- 対象リポジトリへのcommit、push、issue、PR作成
- Cloudflare Worker deploy
- Cloudflare production resourceの作成・更新・削除
- production credentialの使用

## Safety rules

1. 実credentialは使わず、一意なsynthetic canaryのみを使う。
2. 実行ごとに`TEST_ID=opz-cf-e2e-<timestamp>-<pid>`を生成する。
3. 専用test vaultを`OPZ_TEST_VAULT`で明示し、未指定時は開始しない。
4. 作成item名はすべて`$TEST_ID-`で始める。
5. 最初のresource作成前にcleanup trapを登録する。
6. secret-bearing commandではshell tracingを有効にしない。
7. `op item get --reveal`を使わず、secret値を標準出力へ表示しない。
8. `wrangler deploy`、`wrangler secret put`、Cloudflare mutation APIは呼ばない。
9. cloneしたrepositoryは読み取り用途とし、最後にcleanであることを確認する。
10. item削除は、このrunで記録したitem IDだけを対象にする。titleの広い部分一致削除は禁止する。

## Preconditions

以下をresource作成前に確認する。

- `gh auth status`が成功する。
- `op`が認証済みである。
- `OPZ_TEST_VAULT`が存在するtest専用vaultを指す。
- `cargo`、`git`、`gh`、`op`、`jq`、`rg`が利用できる。
- `opz` source treeが、このissue file以外についてcleanである。
- build対象commitを記録する。

いずれかを満たさない場合、itemやcloneを作成せず中止する。

## Ephemeral environment

```sh
set -eu

: "${OPZ_TEST_VAULT:?set a dedicated test vault}"

TEST_ID="opz-cf-e2e-$(date +%Y%m%d-%H%M%S)-$$"
TEST_ROOT="$(mktemp -d "${TMPDIR:-/tmp}/${TEST_ID}.XXXXXX")"
chmod 700 "$TEST_ROOT"
mkdir -p "$TEST_ROOT"/{bin,repos,fixtures,output,state}

cargo build --locked
cp target/debug/opz "$TEST_ROOT/bin/opz"
OPZ_BIN="$TEST_ROOT/bin/opz"

ITEM_INVENTORY="$TEST_ROOT/state/item-ids"
: > "$ITEM_INVENTORY"
chmod 600 "$ITEM_INVENTORY"

gh repo clone f4ah6o/shuttle-rs "$TEST_ROOT/repos/shuttle-rs"
gh repo clone f4ah6o/local-mcp "$TEST_ROOT/repos/local-mcp"
```

各clone直後に以下を保存する。

```sh
git -C "$TEST_ROOT/repos/shuttle-rs" rev-parse HEAD
git -C "$TEST_ROOT/repos/local-mcp" rev-parse HEAD
git -C "$TEST_ROOT/repos/shuttle-rs" status --porcelain
git -C "$TEST_ROOT/repos/local-mcp" status --porcelain
```

statusが空でない場合はテストを開始しない。

## Fixtures

`$TEST_ROOT/fixtures`以下へmode `0600`で作成する。

- API token: `cf_api_token_OPZ_CANARY_<TEST_ID>`
- Worker secret A: `worker_secret_a_OPZ_CANARY_<TEST_ID>`
- Worker secret B: `worker_secret_b_OPZ_CANARY_<TEST_ID>`
- update用の置換値
- nested API response JSON

API response fixtureには以下のfieldを含める。

- `Authorization`
- `Cookie`
- `setCookie`
- `token`
- `apiToken`
- `accesstoken`
- `secret`
- `clientsecret`
- `key`
- `api_key`
- `apikey`
- redact対象外のpublic field

command output検証ではlive Cloudflare APIを呼ばず、test root内のfixture scriptまたは`printf`からsynthetic dataを出力する。

## Cleanup design

cleanupは最初のresource作成前に登録し、途中失敗しても残りの削除を継続する。

```sh
cleanup() {
  set +e

  if [ -f "$ITEM_INVENTORY" ]; then
    while IFS= read -r item_id; do
      [ -n "$item_id" ] || continue
      op item delete "$item_id" \
        --vault "$OPZ_TEST_VAULT" \
        --archive >/dev/null 2>&1 || true
    done < "$ITEM_INVENTORY"
  fi

  rm -rf -- "$TEST_ROOT"
  unset TEST_ID TEST_ROOT OPZ_BIN ITEM_INVENTORY
}

trap cleanup EXIT HUP INT TERM
```

実装時は利用中の`op` CLI仕様に合わせて、archiveまたは完全削除を選択する。最終確認ではtest vaultに`$TEST_ID-`prefixのitemが残っていないことを確認する。

cleanup対象:

1. testで作成した1Password item
2. raw response item
3. fixture
4. captured output
5. item inventory
6. `shuttle-rs` clone
7. `local-mcp` clone
8. test root全体
9. test用環境変数

cleanupはidempotentであること。cleanup不完全なら、機能テストが通っていても全体を失敗と判定する。

## Execution matrix

以下を両repositoryのclone内で個別に実行する。item名にはrepository識別子を含める。

### 1. Dry-run / stdin / API token

```sh
printf '%s' "$SYNTHETIC_API_TOKEN" |
  "$OPZ_BIN" --vault "$OPZ_TEST_VAULT" cloudflare-credential \
    --preset api-token \
    --mode create \
    --item "$TEST_ID-<repo>-api-token" \
    --section Cloudflare \
    --field CLOUDFLARE_API_TOKEN \
    --stdin \
    --dry-run
```

確認:

- exit 0
- create予定と`op://` targetが表示される
- canary値は表示されない
- itemは作成されない

### 2. Create / command output / API token

`--`以降のlocal commandからsynthetic tokenを出力する。

確認:

- 指定vaultに`API_CREDENTIAL` itemが作成される
- 指定section/fieldが`CONCEALED`
- secretがargvとCLI出力に存在しない
- `op://` referenceが返る
- 作成直後にitem IDをcleanup inventoryへ記録する

### 3. Upsert update / stdin / API token

同じitemへ別canaryを投入する。

確認:

- item IDが変わらない
- target fieldが重複せず更新される
- unrelated field/sectionが保持される
- old/new secretが出力されない

### 4. Create / JSON file / Worker secrets

```sh
"$OPZ_BIN" --vault "$OPZ_TEST_VAULT" cloudflare-credential \
  --preset worker-secret \
  --mode create \
  --item "$TEST_ID-<repo>-worker-secrets" \
  --section "Worker Secrets" \
  --file "$TEST_ROOT/fixtures/worker-secrets.json"
```

確認:

- JSON objectの各memberが個別の`CONCEALED` fieldになる
- labelが保持される
- valueはargv/出力へ現れない
- 各fieldのreferenceが返る

### 5. Update / Worker secrets

既存値2件を変更し、新規fieldを1件追加する。

確認:

- matching fieldは重複せず更新される
- new fieldは同一sectionへ追加される
- unrelated fieldは保持される
- `API_CREDENTIAL`以外のitem更新はmutationなしで拒否される

### 6. Command output / redacted API response

nested fixture JSONをlocal command stdoutから取り込む。

確認:

- Authorization、Cookie、token、secret、key系fieldが再帰的に`[REDACTED]`となる
- camelCase、snake_case、hyphenated、lowercase concatenated名を含む
- public fieldは保持される
- canaryはCLI出力と通常ログに残らない
- response fieldは`CONCEALED`

### 7. Explicit raw API response

同じresponseを別itemへ`--raw`で保存する。

確認:

- `--raw`は`api-response`でのみ成功する
- raw内容は指定1Password item以外へ残らない
- CLI出力はraw値を表示しない
- raw item IDをcleanup inventoryへ記録する
- `api-token`または`worker-secret`と`--raw`の組合せはmutation前に失敗する

### 8. Validation failures

各repositoryで以下を確認する。

- input sourceなし
- input source複数指定
- 空item名
- invalid API-response JSON
- empty API token
- empty Worker-secret object
- existing itemへの`--mode create`
- missing itemへの`--mode update`
- stderrにcanaryを出すsource commandの失敗

確認:

- nonzero exit
- 意図しないitem作成・更新なし
- source commandのstderr本文とcanaryがdiagnosticへ出ない

### 9. Repository integrity

各cloneで以下を確認する。

```sh
git status --porcelain
git rev-parse HEAD
```

合格条件:

- statusが空
- clone直後のHEADと同一
- fixture、credential、log、generated configがclone内にない

## Leak checks

全canaryについて、test-owned outputを検索する。

```sh
rg -F "$CANARY" "$TEST_ROOT/output"
```

通常出力ではmatch 0件とする。

raw fixtureは明示raw test中だけprivate test rootに存在してよいが、最終確認前に削除する。

`op` invocationについて以下を確認する。

- secret値がargvにない
- `op item create/edit`のitem templateはstdin経由
- 成功時のstdoutはreferenceと非機密metadataのみ
- stdin templateをdebug logへ保存しない

## Final verification

cleanup実行後に以下を確認する。

- `$OPZ_TEST_VAULT`に`$TEST_ID-`prefixのitemが残っていない
- `$TEST_ROOT`が存在しない
- 2つのcloneが存在しない
- original `opz` repository内にcanaryがない
- original repositoryはissue file以外clean
- Cloudflare resourceを作成・更新していない

## Acceptance criteria

- `f4ah6o/shuttle-rs`と`f4ah6o/local-mcp`の両cloneでmatrixが完了する
- cloneには必ず`gh repo clone`を使う
- 3入力方式、3preset、create/update/upsert、dry-run、custom target、redact、rawを検証する
- synthetic secretがargv、通常ログ、repository file、CLI outputへ漏れない
- cloneしたrepositoryに変更がない
- testで作成したitem、clone、fixture、output、temporary directoryを全削除する
- cleanup不完全時は失敗とする
- secret値を含まない簡潔なtest reportを残す

## Execution deliverables

- repeatable test scriptまたはone-shot harness
- failure-safeでidempotentなcleanup
- 非機密metadataだけのtest evidence
- defectが見つかった場合は`opz/issues/open`へ別issue fileを作成する
- clone先repositoryへremote issueやPRは作成しない
