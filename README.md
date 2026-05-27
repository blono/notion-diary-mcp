# notion-diary-mcp

LLM で書いた日記を **Notion の「日記」データベース** に直接保存する、Rust 製 MCP サーバです。

[Notion 公式 API](https://developers.notion.com/) と [Model Context Protocol](https://modelcontextprotocol.io/) を使い、追記のみに近い保守的な設計で、LLM が暴走しても既存の日記を破壊しにくい構造になっています。

---

## 特徴

- **2 種類の transport に対応**
  - **stdio**: Claude Desktop 等のローカル MCP クライアント向け
  - **Streamable HTTP**: claude.ai / ChatGPT 等のリモート MCP クライアント、および Google Cloud Run などでのホスティング向け
- **Rust 製**
  - 単一バイナリ、依存ランタイム不要、即時起動
- **安全設計**
  - 日記 DB 以外には触れない。過去日の上書きは拒否、今日分のみ置き換え可
- **以下の Markdown に対応**
  - 見出し / 強調 / リスト / 引用 / コード / リンク / チェックボックス / 区切り線
- **過去日記の参照**
  - `get_recent_diary` で直近 N 日分を Markdown で取得可能
- **JST**
  - サーバー側はすべて Asia/Tokyo タイムゾーン
- **削除はゴミ箱経由**
  - Notion 上ではアーカイブ扱いなので、誤操作からの復旧も可能

---

## セットアップ

### 1. Notion 側の準備

#### 1-1. 内部コネクトを作成

1. <https://www.notion.so/profile/integrations/internal> を開く
1. 「**+ 新規コネクトを作成する**」をクリック
1. 名前は適当（例: `My Diary MCP`）
1. 作成後、表示される **「アクセストークン」** をコピー（`ntn_...` で始まる）

#### 1-2. 日記用データベースを内部コネクトに共有

> **⚠️ 最重要のセキュリティポイント**
> コンテンツへのアクセスには **日記 DB だけ** を共有すること。ワークスペース全体や他の DB は共有しないこと。
> コンテンツへのアクセスがアクセスできる範囲 = MCP サーバが触れる範囲なので、ここを絞ることが最強の防御。

1. Notion で日記用データベースのページを開く
2. 右上の「**...**」メニュー → 「**接続**」 → 上で作った内部コネクトを選択
3. 確認ダイアログで承認

#### 1-3. データベース ID を取得

データベースのページを開いた状態で URL を確認:

```
https://www.notion.so/00000000000000000000000000000000?v=11111111111111111111111111111111
                     └───────────────┬───────────────┘
                          これが DB ID（32 桁の hex）
```

#### 1-4. データベース構造の前提

このサーバは以下の構造を前提とする:

- DB の各 row が **月ページ** (タイトル例: `2026/05`)
- 月ページ本文は `# YYYY/MM/DD(曜)` の H1 見出しと本文の繰り返し

例:

```
[月ページ "2026/05"]
  # 2026/05/01(金)
  今日は ...

  # 2026/05/02(土)
  ...

  # 2026/05/09(土)
  ...
```

タイトルプロパティ名 (`Name` / `名前` / `タイトル` 等) は **自動検出** されるので、何でも良い。

### 2. ビルド

```bash
git clone https://github.com/blono/notion-diary-mcp.git
cd notion-diary-mcp
cargo build --release
```

### 3. MCP Inspector

```bash
cargo build
npx -y @modelcontextprotocol/inspector target/debug/notion-diary-mcp
```

リリースバイナリは `target/release/notion-diary-mcp` に生成される。

### 4. 環境変数の設定（動作確認用、任意）

開発時にローカルで動作確認したい場合は `.env` を作成:

```bash
cp .env.example .env
# エディタで編集
```

```dotenv
NOTION_TOKEN=ntn_xxxxxxxxxxxxxxxxxxxxxxxxxxxxxx
NOTION_DIARY_DATABASE_ID=00000000000000000000000000000000
```

ターミナルから直接起動:

```bash
./target/release/notion-diary-mcp
```

`info` レベルのログが stderr に流れ、stdin/stdout で MCP プロトコルを待ち受ける状態になれば OK。

### 5. Claude Desktop に登録

Claude Desktop の設定ファイルを開く:

| OS | パス |
|---|---|
| macOS | `~/Library/Application Support/Claude/claude_desktop_config.json` |
| Windows | `%APPDATA%\Claude\claude_desktop_config.json` |
| Linux | `~/.config/Claude/claude_desktop_config.json` |

`mcpServers` に追記:

```json
{
  "mcpServers": {
    "notion-diary": {
      "command": "/絶対パス/notion-diary-mcp/target/release/notion-diary-mcp",
      "env": {
        "NOTION_TOKEN": "ntn_xxxxxxxxxxxxxxxxxxxxxxxxxxxxxx",
        "NOTION_DIARY_DATABASE_ID": "00000000000000000000000000000000"
      }
    }
  }
}
```

> **⚠️ Windows の場合**: バックスラッシュは JSON でエスケープすること (`C:\\Users\\...\\notion-diary-mcp.exe`)。

Claude Desktop を再起動すると `notion-diary` のツールが利用可能になる。

---

## Streamable HTTP transport で起動する

claude.ai / ChatGPT 等のリモート MCP クライアントから使う場合は、HTTP transport で起動する。

### ローカル起動

```bash
# 認証用の資格情報を発行（一度だけ。パスワードは任意の十分長いランダム文字列）
export MCP_AUTH_USER=diary
export MCP_AUTH_PASSWORD=$(openssl rand -base64 64 | tr -d '/+=')

# HTTP transport で起動（デフォルト 0.0.0.0:8765）
./target/release/notion-diary-mcp --transport=http

# bind を変えたい場合
./target/release/notion-diary-mcp --transport=http --bind=127.0.0.1:9000
```

エンドポイント:

| パス | メソッド | 認証 | 用途 |
|---|---|---|---|
| `/mcp` | POST/GET/DELETE | 必須 | MCP プロトコル |
| `/health` | GET | 不要 | ヘルスチェック（Cloud Run 用） |

認証は HTTP Basic 認証で行う。`Authorization: Basic base64(user:password)` ヘッダが期待値と一致しない場合は 401 を返す。

claude.ai に登録する際は **URL にクレデンシャルを埋め込む** 形式（`https://user:password@host/mcp`）で指定すると、claude.ai が自動的に Basic 認証ヘッダを付与してくれる（後述の「claude.ai / ChatGPT に登録」を参照）。

### ⚠️ セキュリティ

- `MCP_AUTH_USER` / `MCP_AUTH_PASSWORD` は **両方必須**。未設定だと起動が拒否される（リモート公開時の事故防止）
- `MCP_AUTH_USER` には `:` を含めることはできない（Basic 認証の仕様上、ユーザー名とパスワードは `:` で区切るため）
- パスワードはできれば 32 文字以上のランダム文字列にする
- `0.0.0.0` で listen するとローカルネットワーク全体からアクセス可能になる。本番では Cloud Run 等で HTTPS 終端する想定（HTTP 平文では Basic 認証のクレデンシャルが盗聴可能）

---

### claude.ai / ChatGPT に登録

claude.ai / ChatGPT のカスタムコネクタには「カスタムヘッダ」を設定する UI が無いため、Basic 認証の資格情報を **URL に埋め込む** 形式で指定する。クライアント側で自動的に `Authorization: Basic ...` ヘッダが付与される。

#### claude.ai（Pro / Max / Team / Enterprise プラン）

カスタマイズ → コネクタ → コネクタを追加

| 項目 | 値 |
|---|---|
| 名前 | `Notion Diary` |
| リモート MCP サーバー URL | `https://<MCP_AUTH_USER の値>:<MCP_AUTH_PASSWORD の値>@<your-service>.abc.run.app/mcp` |
| OAuth Client ID | 空欄 |
| OAuth クライアントシークレット | 空欄 |

#### ChatGPT (Plus / Team / Enterprise / Pro / Business)

設定 → アプリ → 高度な設定 → 開発者モードを有効化 → MCP Server を追加

| 項目 | 値 |
|---|---|
| MCP サーバーの URL | `https://<MCP_AUTH_USER の値>:<MCP_AUTH_PASSWORD の値>@<your-service>.abc.run.app/mcp` |
| 認証 | None（URL に埋め込んだので個別設定は不要） |

> ChatGPT 側で URL 埋め込み形式が認識されるかどうかは、執筆時点で claude.ai と同じ挙動になるか未検証。

---

## ツール仕様

### `save_diary`

日記を保存する。

| 引数 | 型 | 説明 |
|---|---|---|
| `date` | string | "YYYY-MM-DD" 形式(JST)。未来日付は拒否される。 |
| `content` | string | Markdown 本文。**日付見出しは含めない**（サーバ側で自動付与） |

**動作**:

| 状態 | 結果 |
|---|---|
| 月ページが無い | 月ページを自動作成して、日記を追記 |
| 日見出しが無い | 月ページ末尾に追記 |
| 今日分で日見出しが既にある | 本文を置き換え（旧内容はアーカイブ） |
| 過去日で日見出しが既にある | エラー（Notion 側で手動編集してもらう） |

**返り値**（JSON 文字列）:

```json
{
  "action": "appended" | "replaced",
  "page_url": "https://www.notion.so/...",
  "month": "2026/05",
  "date": "2026-05-09",
  "month_page_created": false,
  "message": "2026-05-09 の日記を追記しました。"
}
```

### `get_recent_diary`

直近 N 日分の日記を Markdown で取得する。

| 引数 | 型 | 説明 |
|---|---|---|
| `days` | u32（省略可） | 1〜31 の範囲。今日を含む。省略時 7。 |

**返り値**（JSON 文字列）:

```json
{
  "from": "2026-05-03",
  "to": "2026-05-09",
  "diary_count": 5,
  "markdown": "# 2026/05/03(日)\n..."
}
```

---

## サポートする Markdown 要素

- 見出し H1 / H2 / H3（H4 以降は H3 に丸める）
- 段落
- 太字 `**...**` / 斜体 `*...*` / 取り消し線 `~~...~~` / インラインコード `` `...` ``
- 箇条書き（入れ子可） / 番号付きリスト
- 引用 `> ...`
- コードブロック ` ```lang ... ``` ` （Notion がサポートしない言語は `plain text` に丸める）
- リンク `[text](url)`
- チェックボックス `- [ ]` / `- [x]`
- 区切り線 `---`

**サポート外**（Notion ブロックには変換せず無視 or 丸める）: 表 / 画像 / フットノート / HTML / 数式

---

## 環境変数

| 変数 | 必須 | 説明 |
|---|---|---|
| `NOTION_TOKEN` | ✅ | Notion Internal Integration Token (`ntn_...`) |
| `NOTION_DIARY_DATABASE_ID` | ✅ | 日記用データベースの ID（32 桁 hex） |
| `MCP_AUTH_USER` | HTTP 時のみ ✅ | Streamable HTTP の Basic 認証ユーザー名。`--transport=http` 起動時は必須。`:` を含めることはできない |
| `MCP_AUTH_PASSWORD` | HTTP 時のみ ✅ | Streamable HTTP の Basic 認証パスワード。`--transport=http` 起動時は必須 |
| `MCP_HTTP_BIND` | ❌ | HTTP の bind アドレス。例: `0.0.0.0:8080`。CLI `--bind` が優先 |
| `PORT` | ❌ | Cloud Run が自動設定。`MCP_HTTP_BIND` 未指定時は `0.0.0.0:$PORT` で listen |
| `DIARY_MAX_PAST_DAYS` | ❌ | 保存可能な過去日の上限（日数）。`0` または未設定で無制限。デフォルト `0` |
| `RUST_LOG` | ❌ | ログレベル。例: `info`, `debug`, `notion_diary_mcp=debug` |

---

## 安全性に関する補足

### 多層防御の構造

このサーバは「LLM が暴走しても致命的な事故にならない」よう、4 層で防御している:

1. **Notion 側の共有設定**
   - 内部コネクトは日記 DB だけに共有。他の DB やページには物理的にアクセス不可
2. **コード側の操作制限**
   - `NotionClient` は日記 MCP で必要な操作だけを実装
   - PATCH /pages 等の危険なエンドポイントは存在すらしない
3. **ビジネスルール**
   - 過去日の見出しが既にあれば書き込み拒否
   - 未来日への書き込み拒否
4. **削除はアーカイブ扱い**
   - Notion 上の `DELETE /blocks/{id}` はゴミ箱送り
   - 30 日以内なら復元可能

### 想定リスクと対策

| リスク | 対策 |
|---|---|
| LLM が誤って大量の日記を生成 | レート制限 + 10KB/req の content 上限 |
| 過去日の日記を意図せず上書き | サーバ側で拒否（今日のみ置き換え可） |
| トークン漏洩 | `Config` の Debug は `***REDACTED***`、ログにも出さない |
| Notion API のレート制限 | 429 を 1 度だけリトライ |

---

## トラブルシューティング

### Claude Desktop に表示されない

- Claude Desktop を完全に終了して再起動
- 設定ファイルの JSON 構文ミスをチェック（末尾カンマ等）
- バイナリのパスが絶対パスになっているか確認
- ログ確認: macOS なら `~/Library/Logs/Claude/mcp*.log`

### `Notion API エラー: 401 Unauthorized`

- `NOTION_TOKEN` が正しいか確認
- 内部コネクトの有効期限切れの可能性（再生成して更新）

### `Notion API エラー: 404 Not Found`

- DB が内部コネクトに共有されていない可能性
- Notion 側で「コネクション」設定を再確認

### `DB にタイトルプロパティ (type=title) が見つかりません`

- DB として作成されているか確認（普通のページではダメ）
- DB の表示が「データベース」になっていることを確認

---

## 開発

```bash
# テスト実行
cargo test

# ログレベル debug で起動
RUST_LOG=notion_diary_mcp=debug cargo run

# clippy
cargo clippy --all-targets -- -D warnings

# fmt
cargo fmt
```

### プロジェクト構成

```
src/
├── main.rs                 # エントリポイント（CLI 引数パース、transport 切替）
├── server.rs               # MCP サーバ本体（tool_router / tool_handler）
├── config.rs               # 環境変数からのコンフィグロード
├── error.rs                # ドメインエラー型
├── time_util.rs            # JST 時刻ユーティリティ
├── transport.rs            # 公開モジュール
├── transport/
│   ├── stdio.rs            # stdio transport 起動
│   └── http.rs             # Streamable HTTP transport + Bearer 認証
├── notion.rs / notion/     # Notion API クライアント
├── markdown.rs / markdown/ # Markdown ↔ Notion blocks 変換
└── diary.rs / diary/       # 日記ドメインロジック
    ├── month_page.rs       # 月ページの解決・作成
    ├── heading.rs          # 日付見出しの検出
    ├── save.rs             # save_diary の実装
    └── read.rs             # get_recent_diary の実装
```

### Cloud Run デプロイ関連

```
Dockerfile        # マルチステージビルド (rust:bookworm → debian:bookworm-slim)
.dockerignore     # ビルドコンテキストから除外
```
