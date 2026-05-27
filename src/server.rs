//! MCP サーバ本体。
//!
//! このモジュールでは:
//! - `DiaryServer` 構造体に共有状態（Notion クライアント、Config、Resolver）を保持
//! - `#[tool_router]` で `save_diary` / `get_recent_diary` を MCP ツールとして公開
//! - `#[tool_handler]` 経由で ServerHandler トレイトを実装
//!
//! 設計方針:
//! - tool 関数の責務は「引数の受け取り」「ドメイン関数の呼び出し」「結果の整形」だけに絞る
//!   ビジネスロジックは `crate::diary` 配下に集約済み。
//! - エラーは AppError → McpError に変換し、MCP Client が読みやすい日本語メッセージにする
//! - 戻り値は CallToolResult::success(vec![Content::text(...)]) に
//!   結果サマリの JSON 文字列を入れる形（MCP Client がパースしやすい）

use crate::config::Config;
use crate::diary::{MonthPageResolver, RecentDiary, SaveResult, get_recent_diary, save_diary};
use crate::notion::NotionClient;
use rmcp::handler::server::router::tool::ToolRouter;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{CallToolResult, Content};
use rmcp::{ErrorData as McpError, ServerHandler, schemars, tool, tool_handler, tool_router};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::sync::Arc;

// =============================================================================
// ツール引数の定義
// =============================================================================

/// `save_diary` ツールの引数。
///
/// JsonSchema を導出することで、MCP クライアントに入力スキーマが自動で公開される。
#[derive(Debug, Clone, Deserialize, Serialize, schemars::JsonSchema)]
pub struct SaveDiaryArgs {
    /// 日記の対象日付。"YYYY-MM-DD" 形式（JST 基準）。
    /// 例: "2026-05-09"
    /// 未来日付は拒否される。
    #[schemars(
        description = "日記の対象日付。'YYYY-MM-DD' 形式(JST)。例: '2026-05-09'。未来日付は拒否されます。"
    )]
    pub date: String,

    /// 日記本文 (Markdown)。
    ///
    /// **重要**: 日付見出し（"# 2026/05/09(土)" 等）は含めないでください。
    /// 日付見出しはサーバ側で自動付与されます。
    ///
    /// サポート要素:
    /// - 見出し H1/H2/H3
    /// - 段落
    /// - 太字 (`**...**`) / 斜体 (`*...*`) / 取り消し線 (`~~...~~`) / インライン code (`` `...` ``)
    /// - 箇条書き (入れ子可) / 番号付きリスト
    /// - 引用 (`> ...`)
    /// - コードブロック (` ```lang ... ``` `)
    /// - リンク `[text](url)`
    /// - チェックボックス (`- [ ]` / `- [x]`)
    /// - 区切り線 (`---`)
    #[schemars(
        description = "日記本文(Markdown)。日付見出しは含めないこと（サーバ側で自動付与）。"
    )]
    pub content: String,
}

/// `get_recent_diary` ツールの引数。
#[derive(Debug, Clone, Deserialize, Serialize, schemars::JsonSchema)]
pub struct GetRecentDiaryArgs {
    /// 取得する日数（今日を含む）。1〜31 の範囲。デフォルトは 7。
    /// 例: 7 → 今日を含む直近 7 日分。
    #[schemars(description = "取得する日数（今日を含む、JST 基準）。1〜31 の範囲。省略時は 7。")]
    #[serde(default)]
    pub days: Option<u32>,
}

// =============================================================================
// サーバ本体
// =============================================================================

/// MCP サーバの共有状態。
///
/// rmcp は tool 呼び出し時に &self でこの構造体にアクセスする。
/// 内部は Arc で包んで安価にクローン可能にしておく。
#[derive(Clone)]
pub struct DiaryServer {
    config: Arc<Config>,
    client: NotionClient,
    resolver: Arc<MonthPageResolver>,
    tool_router: ToolRouter<Self>,
}

#[tool_router]
impl DiaryServer {
    /// サーバを構築する。
    pub fn new(config: Config, client: NotionClient) -> Self {
        let resolver = Arc::new(MonthPageResolver::new(
            client.clone(),
            config.database_id.clone(),
        ));
        Self {
            config: Arc::new(config),
            client,
            resolver,
            tool_router: Self::tool_router(),
        }
    }

    // -------------------------------------------------------------------------
    // tool: save_diary
    // -------------------------------------------------------------------------

    /// 日記を Notion に保存する。
    ///
    /// 動作:
    /// - 月ページ ("YYYY/MM") が無ければ自動作成
    /// - その日の見出しが無ければ追記
    /// - 今日分でその日の見出しが既にある場合は本文を置き換え
    /// - 過去日で見出しが既にある場合はエラー（Notion 側で手動編集してもらう）
    #[tool(
        description = "日記を Notion の日記データベースに保存する。日付見出しはサーバ側で自動付与する（例: '# 2026/05/09(土)'）。今日分の再保存は本文を置き換える。過去日で既に見出しがある場合はエラーになる。"
    )]
    async fn save_diary(
        &self,
        Parameters(args): Parameters<SaveDiaryArgs>,
    ) -> Result<CallToolResult, McpError> {
        let result: SaveResult = save_diary(
            &self.config,
            &self.client,
            &self.resolver,
            &args.date,
            &args.content,
        )
        .await
        .map_err(|e| e.into_mcp())?;

        // MCP Client に返す JSON サマリ
        let payload = json!({
            "action": result.outcome.as_str(),
            "page_url": result.page_url,
            "month": result.month,
            "date": result.date,
            "month_page_created": result.month_page_created,
            // ユーザー向けの補足メッセージ（MCP Client がそのまま表示しやすいよう）
            "message": match &result.outcome {
                crate::diary::SaveDiaryOutcome::Appended => {
                    if result.month_page_created {
                        format!(
                            "{} の月ページを作成し、{} の日記を追記しました。",
                            result.month, result.date
                        )
                    } else {
                        format!("{} の日記を追記しました。", result.date)
                    }
                }
                crate::diary::SaveDiaryOutcome::Replaced { deleted_block_count } => format!(
                    "{} の日記を置き換えました（{} ブロックをアーカイブ）。",
                    result.date, deleted_block_count
                ),
            }
        });

        Ok(CallToolResult::success(vec![Content::text(
            payload.to_string(),
        )]))
    }

    // -------------------------------------------------------------------------
    // tool: get_recent_diary
    // -------------------------------------------------------------------------

    /// 直近 N 日分の日記を Markdown で取得する。
    #[tool(
        description = "直近の日記を Markdown 形式で取得する。days で日数を指定（今日を含む、JST 基準、1-31、省略時 7）。"
    )]
    async fn get_recent_diary(
        &self,
        Parameters(args): Parameters<GetRecentDiaryArgs>,
    ) -> Result<CallToolResult, McpError> {
        let days = args.days.unwrap_or(7);
        let recent: RecentDiary = get_recent_diary(&self.client, &self.resolver, days)
            .await
            .map_err(|e| e.into_mcp())?;

        // MCP Client が解釈しやすいように JSON で返す。markdown 本体もフィールドに入れる。
        let payload = json!({
            "from": recent.from.format("%Y-%m-%d").to_string(),
            "to": recent.to.format("%Y-%m-%d").to_string(),
            "diary_count": recent.diary_count,
            "markdown": recent.markdown,
        });

        Ok(CallToolResult::success(vec![Content::text(
            payload.to_string(),
        )]))
    }
}

// =============================================================================
// ServerHandler 実装
// =============================================================================

// name / version / instructions は #[tool_handler] マクロ引数で渡す。
// rmcp 1.x では ServerInfo / Implementation が #[non_exhaustive] のため
// struct literal が使えない。マクロ経由が公式の想定 API。
#[tool_handler(
    name = "notion-diary-mcp",
    version = "0.1.0",
    instructions = "Notion の日記データベースに日記を保存・取得するための MCP サーバ。\n- save_diary: 日記を保存する（今日分は置き換え、過去日で既存はエラー）\n- get_recent_diary: 直近 N 日分の日記を Markdown で取得する\n\n注意: save_diary の content には日付見出しを含めないこと（見出しはサーバ側で自動付与する）。"
)]
impl ServerHandler for DiaryServer {}
