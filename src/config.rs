//! 環境変数からアプリケーション設定を読み込む。
//!
//! 設計方針:
//! - 機密情報 (Notion Token) は絶対に Debug 出力しない
//! - 起動時に必須項目をすべて検証し、不足があれば早期に panic で終了
//! - デフォルト値は安全寄り

use crate::error::{AppError, AppResult};
use std::env;

/// アプリケーション設定。
#[derive(Clone)]
pub struct Config {
    /// Notion Internal Integration Token（機密）
    notion_token: String,

    /// 日記用データベース ID
    pub database_id: String,

    /// 過去日書き込み上限 (日数)。0 で無制限。
    pub max_past_days: i64,

    /// Notion API バージョン（固定で渡す）
    pub notion_api_version: String,
}

impl Config {
    /// 環境変数からロードする。`.env` ファイルがあれば自動で読み込む。
    pub fn from_env() -> AppResult<Self> {
        // .env が存在すれば読み込む。本番運用では MCP の env 設定を使うので不要だが、
        // 開発時には便利。
        let _ = dotenvy::dotenv();

        let notion_token = env::var("NOTION_TOKEN")
            .map_err(|_| AppError::Config("環境変数 NOTION_TOKEN が設定されていません".into()))?;

        if notion_token.trim().is_empty() {
            return Err(AppError::Config("NOTION_TOKEN が空です".into()));
        }

        let database_id = env::var("NOTION_DIARY_DATABASE_ID").map_err(|_| {
            AppError::Config("環境変数 NOTION_DIARY_DATABASE_ID が設定されていません".into())
        })?;
        let database_id = normalize_notion_id(&database_id)?;

        let max_past_days = env::var("DIARY_MAX_PAST_DAYS")
            .ok()
            .and_then(|s| s.parse::<i64>().ok())
            .unwrap_or(0); // デフォルトは無制限 (ユーザー要望)

        Ok(Self {
            notion_token,
            database_id,
            max_past_days,
            notion_api_version: "2022-06-28".to_string(),
        })
    }

    /// HTTP リクエストの Authorization ヘッダ用に Bearer トークンを返す。
    /// この値はログ等に絶対に出さないこと。
    pub fn bearer_token(&self) -> String {
        format!("Bearer {}", self.notion_token)
    }
}

// 機密情報がうっかりログに出るのを防ぐため、独自 Debug 実装。
impl std::fmt::Debug for Config {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Config")
            .field("notion_token", &"***REDACTED***")
            .field("database_id", &self.database_id)
            .field("max_past_days", &self.max_past_days)
            .field("notion_api_version", &self.notion_api_version)
            .finish()
    }
}

/// Notion の ID を正規化する。
///
/// Notion ID はハイフン入り (8-4-4-4-12) もハイフン無し（32 桁）も受け付ける。
/// API 呼び出しではどちらでも通るので、内部では入力値をそのまま保持する形にするが、長さと文字種だけは検証しておく。
fn normalize_notion_id(raw: &str) -> AppResult<String> {
    let trimmed = raw.trim().to_lowercase();
    let stripped: String = trimmed.chars().filter(|c| *c != '-').collect();
    if stripped.len() != 32 || !stripped.chars().all(|c| c.is_ascii_hexdigit()) {
        return Err(AppError::Config(format!(
            "NOTION_DIARY_DATABASE_ID が Notion の ID 形式ではありません（32 桁の 16 進文字列）: {raw}"
        )));
    }
    Ok(stripped)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_id_ハイフン無しを受け付ける() {
        let id = normalize_notion_id("00000000000000000000000000000000").unwrap();
        assert_eq!(id, "00000000000000000000000000000000");
    }

    #[test]
    fn normalize_id_ハイフン入りを受け付ける() {
        let id = normalize_notion_id("00000000-0000-0000-0000-000000000000").unwrap();
        assert_eq!(id, "00000000000000000000000000000000");
    }

    #[test]
    fn normalize_id_不正値を拒否する() {
        assert!(normalize_notion_id("invalid").is_err());
        assert!(normalize_notion_id("0000000000000000000000000000000").is_err()); // 31桁
        assert!(normalize_notion_id("ZZZZ0000000000000000000000000000").is_err()); // 非16進
    }
}
