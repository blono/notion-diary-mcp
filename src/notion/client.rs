//! Notion API への HTTP リクエストを担う薄いラッパクライアント。
//!
//! 設計方針:
//! - 公開メソッドは「日記 MCP で使う操作」だけに限定する。
//!   その他のエンドポイント（PATCH /v1/pages 等）はあえて実装しない。
//!   バグでうっかり呼んでしまうリスクを構造的に排除する。
//! - 429 (rate limit) は短時間 sleep して 1 度だけリトライ。
//! - すべての API 呼び出しは tracing でログ出力（URL とステータスのみ。トークン本体やレスポンス全文は出さない）。

use crate::config::Config;
use crate::error::{AppError, AppResult};
use crate::notion::types::*;
use reqwest::{Client as HttpClient, StatusCode};
use serde_json::{Value, json};
use std::time::Duration;
use tracing::{debug, warn};

const NOTION_BASE: &str = "https://api.notion.com";

/// Notion API クライアント。
#[derive(Clone)]
pub struct NotionClient {
    http: HttpClient,
    config: Config,
}

impl NotionClient {
    pub fn new(config: Config) -> AppResult<Self> {
        let http = HttpClient::builder()
            .timeout(Duration::from_secs(30))
            // ユーザーエージェントは Notion の推奨に従って設定
            .user_agent(concat!("notion-diary-mcp/", env!("CARGO_PKG_VERSION")))
            .build()?;
        Ok(Self { http, config })
    }

    // -------------------------------------------------------------------------
    // 公開 API: 日記 MCP で使う操作のみ
    // -------------------------------------------------------------------------

    /// データベースのスキーマを取得する。
    /// title プロパティ名の自動検出に使用。
    pub async fn get_database(&self, database_id: &str) -> AppResult<DatabaseObject> {
        let url = format!("{NOTION_BASE}/v1/databases/{database_id}");
        let resp = self.send_with_retry(self.http.get(&url)).await?;
        Ok(serde_json::from_value(resp)?)
    }

    /// データベースをクエリして、タイトルが完全一致するページを探す。
    /// 見つかった最初のページを返す（複数あれば運用ミスなので最初のものを採用）。
    pub async fn find_page_by_title(
        &self,
        database_id: &str,
        title_property: &str,
        title: &str,
    ) -> AppResult<Option<PageObject>> {
        let url = format!("{NOTION_BASE}/v1/databases/{database_id}/query");
        let body = json!({
            "filter": {
                "property": title_property,
                "title": {
                    "equals": title
                }
            },
            "page_size": 5
        });
        let resp = self
            .send_with_retry(self.http.post(&url).json(&body))
            .await?;
        let result: QueryDatabaseResponse = serde_json::from_value(resp)?;

        // Notion API のフィルタが期待通りに動かないケースへのフェイルセーフとして、
        // クライアント側でもタイトルの完全一致を確認する。
        // （フィルタが全件返してきた場合でも先頭の別ページを誤返却しない）
        Ok(result.results.into_iter().find(|p| {
            if p.archived {
                return false;
            }
            // properties にタイトルが含まれている場合は照合する。
            // 含まれていない場合（ページ作成直後など）はフィルタ結果を信用してそのまま通す。
            match p.title_text(title_property) {
                Some(t) => t == title,
                None => true,
            }
        }))
    }

    /// データベースに新規ページを作成する（タイトルだけのシンプルなページ）。
    pub async fn create_page_in_database(
        &self,
        database_id: &str,
        title_property: &str,
        title: &str,
    ) -> AppResult<PageObject> {
        let url = format!("{NOTION_BASE}/v1/pages");
        let body = json!({
            "parent": { "database_id": database_id },
            "properties": {
                title_property: {
                    "title": [
                        { "text": { "content": title } }
                    ]
                }
            }
        });
        let resp = self
            .send_with_retry(self.http.post(&url).json(&body))
            .await?;
        Ok(serde_json::from_value(resp)?)
    }

    /// ページ（ブロック）の子ブロックをすべて取得する（ページネーション込み）。
    pub async fn list_all_block_children(&self, block_id: &str) -> AppResult<Vec<BlockResponse>> {
        let mut all = Vec::new();
        let mut cursor: Option<String> = None;
        loop {
            let mut url = format!("{NOTION_BASE}/v1/blocks/{block_id}/children?page_size=100");
            if let Some(c) = &cursor {
                url.push_str(&format!("&start_cursor={c}"));
            }
            let resp = self.send_with_retry(self.http.get(&url)).await?;
            let page: ListBlockChildrenResponse = serde_json::from_value(resp)?;
            all.extend(page.results);
            if !page.has_more {
                break;
            }
            cursor = page.next_cursor;
            if cursor.is_none() {
                break;
            }
        }
        Ok(all)
    }

    /// ブロックの子として複数のブロックを追加する。
    /// `after` を指定すると、その block_id の直後に挿入される（省略時は末尾）。
    ///
    /// Notion API は 1 リクエスト 100 ブロックまでしか受け付けないので、
    /// 100 個ずつチャンク分割して送る。
    pub async fn append_block_children(
        &self,
        parent_block_id: &str,
        children: Vec<Value>,
        after: Option<&str>,
    ) -> AppResult<()> {
        if children.is_empty() {
            return Ok(());
        }
        let url = format!("{NOTION_BASE}/v1/blocks/{parent_block_id}/children");

        // 直前に挿入したブロックの「最後の ID」を、次のチャンクの after に使う。
        // こうすることで、複数チャンクに分けても順序が保たれる。
        let mut current_after = after.map(|s| s.to_string());

        for chunk in children.chunks(100) {
            let mut body = json!({
                "children": chunk,
            });
            if let Some(a) = &current_after {
                body["after"] = json!(a);
            }
            let resp = self
                .send_with_retry(self.http.patch(&url).json(&body))
                .await?;

            // 返却された results の最後のブロック ID を次の after に使う
            let results = resp
                .get("results")
                .and_then(|v| v.as_array())
                .ok_or_else(|| {
                    AppError::UnexpectedStructure(
                        "append_block_children のレスポンスに results がありません".into(),
                    )
                })?;
            if let Some(last) = results.last() {
                if let Some(id) = last.get("id").and_then(|v| v.as_str()) {
                    current_after = Some(id.to_string());
                }
            }
        }
        Ok(())
    }

    /// ブロックを削除する（実際は Notion 上で「アーカイブ」され、ゴミ箱に入る）。
    /// 取り返しがつくので、操作ミスがあっても復旧できる。
    pub async fn delete_block(&self, block_id: &str) -> AppResult<()> {
        let url = format!("{NOTION_BASE}/v1/blocks/{block_id}");
        warn!(target: "notion_diary_mcp::audit", block_id = %block_id, "delete_block を実行します");
        self.send_with_retry(self.http.delete(&url)).await?;
        Ok(())
    }

    // -------------------------------------------------------------------------
    // 内部: 送信 + リトライ + エラーハンドリング
    // -------------------------------------------------------------------------

    /// 共通リクエスト送信。
    ///
    /// - Authorization / Notion-Version ヘッダを付ける
    /// - 429 を 1 度だけリトライする
    /// - 4xx/5xx は AppError に変換する
    async fn send_with_retry(&self, req: reqwest::RequestBuilder) -> AppResult<Value> {
        // 1 度だけリトライするために確保。
        let req_for_retry = req.try_clone();
        let mut resp = self.send_once(req).await?;

        if resp.status() == StatusCode::TOO_MANY_REQUESTS {
            if let Some(retry_req) = req_for_retry {
                let wait_ms = resp
                    .headers()
                    .get("Retry-After")
                    .and_then(|v| v.to_str().ok())
                    .and_then(|s| s.parse::<u64>().ok())
                    .map(|s| s * 1000)
                    .unwrap_or(1000);
                warn!(
                    wait_ms,
                    "Notion API rate limited (429), 待機後にリトライします"
                );
                tokio::time::sleep(Duration::from_millis(wait_ms.min(10_000))).await;
                resp = self.send_once(retry_req).await?;
            }
        }

        let status = resp.status();
        let url = resp.url().clone();
        let text = resp.text().await?;
        debug!(target: "notion_diary_mcp::http", method = "?", url = %url, status = %status);

        if !status.is_success() {
            return Err(AppError::NotionApi(format!(
                "{} {} -> {}",
                status,
                url,
                truncate(&text, 500)
            )));
        }

        let value: Value = serde_json::from_str(&text)?;
        Ok(value)
    }

    /// 1 回の送信処理（ヘッダ付与のみ）。
    async fn send_once(&self, req: reqwest::RequestBuilder) -> AppResult<reqwest::Response> {
        let resp = req
            .header("Authorization", self.config.bearer_token())
            .header("Notion-Version", &self.config.notion_api_version)
            .send()
            .await?;
        Ok(resp)
    }
}

/// 長すぎるエラー本文を切り詰める。
fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let truncated: String = s.chars().take(max).collect();
        format!("{truncated}…（以下省略）")
    }
}
