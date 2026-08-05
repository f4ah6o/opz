Proposal: Environment限定Service AccountをブラウザRPAで生成するCLI

概要

opz に、1Password Service AccountをブラウザRPA経由で作成し、特定の1Password Environmentだけを読み取れる状態で利用する機能を追加する。

対象Gitリポジトリを自動検出し、リポジトリに対応するEnvironmentとService Accountを関連付ける。

主な利用例は次のとおり。

opz service-account create --repo
opz service-account run --repo -- cargo test
opz service-account revoke --repo

Service Account tokenやEnvironment内の秘密値を、リポジトリ、設定ファイル、コマンド引数、ログへ平文保存しない。

背景

opz はすでに以下をサポートしている。

* 1Password Environmentの作成、一覧、変数管理
* opz run --environment によるEnvironment変数の注入
* Git remoteを利用したリポジトリ自動検出
* 1PasswordアイテムとGitHubリポジトリの関連付け
* 秘密値をargvやログへ残さない実行モデル

一方、1Password EnvironmentへアクセスできるService Accountの作成は、1Password CLIだけでは完結せず、1Password.comのService Account作成画面を操作する必要がある。

そのため、ブラウザ操作を手動作業として残すのではなく、opz に限定的なブラウザRPA機能を内蔵し、CLIからEnvironment限定Service Accountを生成できるようにする。

目的

* Gitリポジトリごとに利用可能なクレデンシャルを分離する
* 対象Environment以外へのService Accountアクセスを許可しない
* Service Account作成時のブラウザ操作をCLIから自動化する
* Service Account tokenをOSの資格情報ストアへ安全に保存する
* 実行対象コマンドへService Account token自体を渡さない
* Service Accountの作成、確認、利用、失効をopzで一元管理する
* LLMやCIからも安全に操作できるCLIインターフェースを提供する

非目的

* 1Passwordのログイン、MFA、SSO、Passkey認証の回避
* CAPTCHAやセキュリティ確認の突破
* 任意のWebサイトを操作する汎用RPAフレームワークの実装
* Service Accountに複数Environmentや広範なVault権限を自動付与すること
* Service Account tokenをリポジトリや.envへ保存すること
* 1Password Environment内の秘密値をopzの管理コマンドで表示すること

想定するセキュリティモデル

リポジトリと1Password Environmentを原則として1対1で対応させる。

Git repository
    opz-rs/opz
        │
        ▼
1Password Environment
    opz-rs-opz
        │
        ▼
Service Account
    opz-opz-rs-opz

Service Accountには、対象Environmentへの読み取りアクセスだけを付与する。

Vaultへのアクセスはデフォルトで付与しない。

実行時はService Account tokenを1Password CLIにだけ渡し、最終的に起動する子プロセスからは除外する。

OS credential store
        │
        │ Service Account token
        ▼
1Password CLI
        │
        │ Environment variables
        ▼
opz
        │
        │ Environment variables only
        ▼
target command

CLI案

Service Accountの作成

opz service-account create --repo

現在のGit remoteからリポジトリを検出し、対応するEnvironmentを選択してService Accountを作成する。

Environmentを明示する場合:

opz service-account create \
  --environment opz-rs-opz

名前を明示する場合:

opz service-account create \
  --environment opz-rs-opz \
  --name opz-opz-rs-opz

ブラウザを表示せずに実行する場合:

opz service-account create \
  --repo \
  --headless

ただし、ログイン、MFA、SSO、Passkeyなどの対話が必要な場合は、可視ブラウザへフォールバックするか、認証が必要であることを示して安全に停止する。

Service Accountを使った実行

opz service-account run --repo -- cargo test

Environmentを明示する場合:

opz service-account run \
  --environment opz-rs-opz \
  -- cargo test

既存のopz run --environmentとの統合も検討する。

opz run \
  --environment opz-rs-opz \
  --service-account repo \
  -- cargo test

状態確認

opz service-account status --repo

表示可能な情報:

* リポジトリ名
* Environment名とID
* Service Account名
* Service Account ID
* tokenの保存先種別
* tokenが存在するか
* Service AccountでEnvironmentを読み取れるか
* 最終検証日時

tokenやEnvironmentの値は表示しない。

失効

opz service-account revoke --repo

ブラウザRPAで対象Service Accountを失効または削除し、ローカルのtokenもOS資格情報ストアから削除する。

ローカルtokenだけを削除する場合:

opz service-account forget --repo

dry-run

opz service-account create --repo --dry-run

dry-runでは以下だけを表示する。

* 検出したリポジトリ
* 対象Environment
* 作成予定のService Account名
* 実行予定のブラウザ操作
* tokenの保存先識別子

ブラウザ起動、Service Account作成、token取得、秘密値解決は行わない。

リポジトリ検出

--repo指定時は、現在のGit remoteから正規化済みのリポジトリ名を取得する。

例:

git@github.com:opz-rs/opz.git
https://github.com/opz-rs/opz.git

いずれも以下へ正規化する。

opz-rs/opz

デフォルト命名規則:

Environment:
  opz-rs-opz
Service Account:
  opz-opz-rs-opz

命名規則は将来的に設定可能とするが、初期実装では決定論的な固定ルールを採用する。

ローカル設定

リポジトリには秘密を含まない関連付け情報だけを保存する。

例:

version = 1
[service-account]
repository = "opz-rs/opz"
environment_id = "..."
environment_name = "opz-rs-opz"
service_account_id = "..."
service_account_name = "opz-opz-rs-opz"
token_store = "system-keyring"
token_key = "opz/service-account/opz-rs/opz"

候補パス:

.opz/service-account.toml

保存してはならない情報:

* Service Account token
* Environment変数の値
* 1Passwordセッション情報
* Cookie
* Authorization header
* ブラウザのlocalStorage内容
* DOMから取得した秘密値

Token保存

Service Account tokenはOSの資格情報ストアへ保存する。

* macOS: Keychain
* Windows: Credential Manager
* Linux: Secret Service
* 非対話CI: stdinまたは外部Secret Store

Rustの候補依存:

keyring = "..."
zeroize = "..."

tokenはメモリ上でも秘密値型で扱い、DebugとDisplayで必ずredactする。

tokenを文字列としてエラーコンテキストやトレースへ含めない。

ブラウザRPA方式

ブラウザ一式をバイナリへ埋め込まず、ローカルにインストール済みの対応ブラウザを利用する。

優先候補:

1. Google Chrome
2. Chromium
3. Microsoft Edge

Chrome DevTools Protocolを利用して操作する。

Rustの実装候補:

chromiumoxide = "..."
tokio = { version = "...", features = ["rt-multi-thread"] }

専用ブラウザプロファイルを作成し、通常利用しているブラウザプロファイルとは分離する。

例:

~/.local/share/opz/browser-profile/

プラットフォーム別に適切なユーザーデータディレクトリへ配置する。

RPA操作フロー

1. Git remoteから対象リポジトリを特定する
2. 対応するEnvironment名またはIDを解決する
3. Environmentが存在しない場合は、既存のEnvironment機能で作成する
4. 対応ブラウザを専用プロファイルで起動する
5. 1Password.comへ移動する
6. ログイン状態を確認する
7. 認証が必要な場合はユーザー操作を待つ
8. Service Account作成画面へ移動する
9. Service Account名を入力する
10. Vaultアクセスを選択しない
11. 対象Environmentだけを選択する
12. 確認画面の内容を読み取る
13. Environmentが一つだけ選択されていることを再検証する
14. Vault権限が付与されていないことを再検証する
15. Service Accountを作成する
16. 一度だけ表示されるtokenを取得する
17. tokenを即座にOS資格情報ストアへ保存する
18. tokenを使用してEnvironment読み取りを検証する
19. token表示画面を閉じる
20. DOM参照、クリップボード、メモリ上の一時値を破棄する

UI要素の特定方法

1Password.comの内部CSSクラスやDOM構造へ強く依存しない。

要素の探索優先順位:

1. ARIA role
2. accessible name
3. aria-label
4. <label>とフォーム入力の関連
5. 表示テキスト
6. 安定した属性
7. CSS selector

難読化されたCSSクラスや位置ベースのクリックは可能な限り使用しない。

fail-closed要件

次の場合は作成ボタンを押さずに停止する。

* 対象Environmentを一意に特定できない
* 複数Environmentが選択されている
* Vault権限が付与されている
* 確認画面からService Account名を検証できない
* 想定外の権限項目が表示される
* UIの構造が既知の状態と一致しない
* token表示画面を検証できない
* token保存先を利用できない
* 既存Service Accountとの衝突を解決できない

失敗時にスクリーンショットやHTMLを保存する場合は、token、Cookie、Authorization情報、秘密値を必ずredactする。

デフォルトでは秘密を含む可能性があるHTML全体を保存しない。

実行時の秘密値注入

Service Account tokenを設定して1Password CLIを実行する。

OP_SERVICE_ACCOUNT_TOKEN=<token>
op environment read <environment>

ただし、最終的な対象コマンドにはOP_SERVICE_ACCOUNT_TOKENを継承させない。

概念実装:

let token = token_store.load(&token_key)?;
let output = Command::new("op")
    .arg("environment")
    .arg("read")
    .arg(environment_id)
    .env("OP_SERVICE_ACCOUNT_TOKEN", token.expose_secret())
    .output()?;
let variables = parse_environment_output(output)?;
let mut child = Command::new(program);
child.args(args);
child.env_remove("OP_SERVICE_ACCOUNT_TOKEN");
child.envs(variables);
child.status()?;

実際の実装では、秘密値がargv、ログ、エラー、トレースへ含まれないことを保証する。

既存Environment機能との関係

Environment自体の管理には既存の1Password MCP連携を使用する。

RPAを利用する範囲は、CLIまたはMCPで提供されていない操作に限定する。

* Environment一覧: 既存MCP
* Environment作成: 既存MCP
* Environment変数名確認: 既存MCP
* Environmentへのplaceholder追加: 既存MCP
* Service Account作成: ブラウザRPA
* Service Account失効: ブラウザRPA
* Environment変数の実行時読み取り: 1Password CLI
* token保存: OS資格情報ストア

既存セキュリティポリシーへの追加

以下をdocs/security.mdへ追加する。

* ブラウザRPAは1Password.comのService Account管理画面だけを対象とする
* Service Account tokenをargvへ含めない
* Service Account tokenを子コマンドへ継承しない
* token表示画面のHTML、スクリーンショット、ログを永続保存しない
* ブラウザのCookieやセッションデータを出力しない
* 専用ブラウザプロファイルの権限を制限する
* RPA失敗時は秘密を含まない固定メッセージを返す
* dry-runではブラウザを起動しない
* 作成直前に対象Environmentと権限を再検証する
* 想定外の権限が存在する場合はfail closedとする
* token取得後は可能な範囲でメモリをzeroizeする

想定モジュール構成

src/
├── service_account/
│   ├── mod.rs
│   ├── cli.rs
│   ├── config.rs
│   ├── naming.rs
│   ├── repository.rs
│   ├── token_store.rs
│   ├── runner.rs
│   └── rpa/
│       ├── mod.rs
│       ├── browser.rs
│       ├── cdp.rs
│       ├── locators.rs
│       ├── onepassword.rs
│       └── redact.rs

必要に応じて既存のGit remote検出、Environment解決、SecretValue、Redactorを再利用する。

実装フェーズ

Phase 1: 設計と安全基盤

* Service Account用CLI構造を追加
* リポジトリとEnvironmentの命名規則を実装
* 設定ファイル形式を定義
* OS資格情報ストア抽象化を追加
* tokenのredactとzeroizeを追加
* dry-runを実装

Phase 2: RPAの観測モード

* ブラウザ検出
* 専用プロファイル起動
* 1Password.comへの遷移
* ログイン状態判定
* Service Account作成画面の読み取り
* 作成ボタンは押さない観測専用モード
* UI構造のfixture化とテスト

Phase 3: Service Account作成

* Service Account名入力
* Environment選択
* Vault未選択の検証
* 確認画面の再検証
* 作成実行
* token取得
* OS資格情報ストアへの保存
* tokenによるEnvironment読み取り検証

Phase 4: 実行統合

* service-account runを実装
* tokenを1Password CLIだけへ渡す
* 子コマンドからtokenを除外
* Environment変数だけを子プロセスへ注入
* 既存opz run --environmentとの統合を検討

Phase 5: 管理機能

* status
* forget
* revoke
* 既存Service Accountの検出
* 再作成またはローテーション
* ドキュメントとAgent Skillの更新

テスト方針

ユニットテスト

* Git remote正規化
* Environment名生成
* Service Account名生成
* token key生成
* 設定ファイルの読み書き
* tokenのDebugとDisplayのredact
* 子プロセスからOP_SERVICE_ACCOUNT_TOKENを除外
* 複数Environment選択時の拒否
* Vault権限検出時の拒否
* dry-runで副作用が発生しないこと

RPA fixtureテスト

1Password.comの実画面へ常時依存せず、保存した非秘密HTML fixtureでlocatorをテストする。

fixtureには以下を含めない。

* token
* Cookie
* Authorization情報
* アカウント固有情報
* Environment変数値
* 個人情報

テスト対象:

* Service Account作成画面の検出
* Environment一覧の検出
* 対象Environmentの選択
* Vault権限未選択の検証
* 確認画面の解析
* token表示画面の検出
* UI変更時のfail-closed

統合テスト

実アカウントを使うテストは明示的なfeatureまたは環境変数がある場合だけ実行する。

例:

OPZ_E2E_1PASSWORD=1

テスト用EnvironmentとService Accountには一意な接頭辞を付ける。

opz-e2e-<timestamp>-<random>

テスト終了時はService AccountとEnvironmentを削除する。

cleanupに失敗した場合は、削除対象IDだけを秘密を含まない形式で出力する。

受け入れ条件

* opz service-account create --repoでGitリポジトリを検出できる
* 対応するEnvironmentを一意に解決できる
* Environmentが存在しない場合に作成できる
* 1Password.comを専用ブラウザプロファイルで開ける
* 必要な場合にユーザーがログイン、MFA、SSO、Passkeyを完了できる
* 対象Environmentだけを選択できる
* Vaultアクセスが付与されていないことを作成直前に検証できる
* 想定外のUIまたは権限状態では作成せず停止する
* Service Account tokenを一度だけ取得できる
* tokenをOS資格情報ストアへ保存できる
* tokenが設定ファイル、ログ、argvへ出力されない
* opz service-account run --repo -- <COMMAND>でEnvironment変数を注入できる
* 実行対象コマンドへOP_SERVICE_ACCOUNT_TOKENが渡らない
* statusで秘密を表示せず接続状態を確認できる
* forgetでローカルtokenを削除できる
* revokeでService Accountを失効できる
* dry-runではブラウザ起動や秘密値取得が発生しない
* macOS、Windows、Linuxでtoken保存層が動作する
* 既存のopz run、Environment、item、plugin機能を壊さない
* セキュリティドキュメントとREADMEが更新される

リスク

1Password.comのUI変更

ブラウザRPAはUI変更の影響を受ける。

対策:

* ARIA roleとaccessible nameを優先する
* CSSクラス依存を避ける
* locatorを一箇所へ集約する
* fixtureテストを用意する
* UI不一致時はfail closedとする
* RPA実装を独立モジュールとして交換可能にする

Service Account tokenの漏えい

tokenは対象Environmentの秘密値へアクセスできるため、高機密情報として扱う必要がある。

対策:

* OS資格情報ストアへ保存する
* argvへ含めない
* ログへ出さない
* 子コマンドへ継承しない
* SecretValueとRedactorを利用する
* token取得画面を保存しない
* メモリ上の一時値を可能な範囲でzeroizeする

ブラウザセッションの漏えい

専用プロファイルには1Password.comのログインセッションが保存される可能性がある。

対策:

* 専用プロファイルを使用する
* ファイル権限を制限する
* プロファイルパスをログへ過剰に出さない
* opz service-account browser-resetなどの削除手段を検討する
* 通常ブラウザの既存プロファイルを操作しない

誤ったEnvironmentへのアクセス付与

名前の類似やUI変更により、別Environmentを選択する可能性がある。

対策:

* 可能ならEnvironment IDも照合する
* 選択前後で名前とIDを検証する
* 作成確認画面で再検証する
* 複数選択時は拒否する
* 作成後にService Accountで対象Environmentを読み取って検証する

検討事項

* Service Account tokenの有効期限やローテーションをどのように扱うか
* Service Account作成後に権限変更が必要になった場合、再作成を標準動作にするか
* service-account runを独立コマンドにするか、既存のrun --environmentへ統合するか
* Git worktreeやmonorepoでEnvironmentをどの単位にするか
* GitHub ActionsなどのCIへtokenを安全に転送する仕組みを追加するか
* 1Passwordが将来Environment対応Service Account作成APIまたはCLIを提供した場合、RPAから公式APIへ置き換える抽象化を設けるか
* headless実行を正式サポートするか、可視ブラウザをデフォルトとするか

推奨方針

初期実装では以下に限定する。

* 1リポジトリにつき1Environment
* 1Environmentにつき1Service Account
* Vaultアクセスなし
* Environment読み取りのみ
* 可視ブラウザをデフォルト
* tokenはOS資格情報ストアへ保存
* Service Account tokenは対象コマンドへ渡さない
* UI不一致時は必ず停止
* Service Account作成と失効だけをRPA対象とする
* Environment管理は既存MCPを利用する

この限定構成で安全性と実用性を確認した後、ローテーション、CI連携、monorepo対応を別Issueとして拡張する。
