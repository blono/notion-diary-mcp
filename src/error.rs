//! アプリ全体で使用するエラー型を定義する。
//!
//! 設計方針:
//! - ライブラリ内部では `thiserror` で型付きエラーを定義 (`AppError`)
//! - main / tools 層では `anyhow` で扱い、最終的に MCP エラーへ変換する
//! - MCP クライアントに返すメッセージは「人間が次に何をすればいいか」
//!   が分かるように、なるべく具体的な日本語で返す

use rmcp::ErrorData as McpError;
use thiserror::Error;

/// アプリケーションのドメインエラー。
#[derive(Debug, Error)]
pub enum AppError {
    /// 入力バリデーションエラー（MCP からの入力が不正）
    #[error("入力エラー: {0}")]
    InvalidInput(String),

    /// 過去日かつ既存の見出しがある場合の上書き拒否エラー
    #[error(
        "過去日 {date} の日記は既に存在します。上書きはこのツールではできません。Notion 側で手動編集してください。"
    )]
    PastDateAlreadyExists { date: String },

    /// 未来日への書き込み拒否エラー
    #[error("未来日付 ({date}) への書き込みはできません。日付を確認してください。")]
    FutureDate { date: String },

    /// 過去日上限を超えた場合
    #[error("過去日上限を超えています ({date} は {max_days} 日より前)。")]
    PastDateTooFar { date: String, max_days: i64 },

    /// 設定エラー（環境変数の欠如など）
    #[error("設定エラー: {0}")]
    Config(String),

    /// Notion API 関連のエラー
    #[error("Notion API エラー: {0}")]
    NotionApi(String),

    /// HTTP / ネットワークエラー
    #[error("HTTP エラー: {0}")]
    Http(#[from] reqwest::Error),

    /// JSON パースエラー
    #[error("JSON パースエラー: {0}")]
    Json(#[from] serde_json::Error),

    /// 想定外の構造（Notion から予期しないデータが返ってきた等）
    #[error("不正な構造: {0}")]
    UnexpectedStructure(String),
}

impl AppError {
    /// MCP のエラーレスポンスへ変換する。
    ///
    /// MCP Client には `error.message` が返るので、ユーザーフレンドリーな
    /// 日本語メッセージになるよう Display 実装をそのまま使う。
    pub fn into_mcp(self) -> McpError {
        // invalid_params 系か、それ以外の internal_error 系かを区別する
        match self {
            AppError::InvalidInput(_)
            | AppError::FutureDate { .. }
            | AppError::PastDateTooFar { .. } => McpError::invalid_params(self.to_string(), None),
            AppError::PastDateAlreadyExists { .. } => {
                // ビジネスルール違反は invalid_params で返す
                // （MCP Client が「過去日の上書きはダメなんだな」と学習しやすい）
                McpError::invalid_params(self.to_string(), None)
            }
            _ => McpError::internal_error(self.to_string(), None),
        }
    }
}

/// アプリ全体で使う Result のエイリアス。
pub type AppResult<T> = Result<T, AppError>;
