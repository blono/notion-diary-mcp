//! Notion API のレスポンス型（必要分のみ）を定義する。
//!
//! 設計方針:
//! - すべての block type を網羅するのは現実的でないため、
//!   block_type の文字列とそれ以外のフィールドは flatten で受け取る。
//! - rich_text 用の最小限の型は定義しておく（日記では多用するため）。

use serde::{Deserialize, Serialize};

/// ブロック取得レスポンスの最小型。
///
/// - `id`: ブロック ID（削除や挿入位置指定に使う）
/// - `block_type`: "heading_1", "paragraph" 等
/// - `has_children`: 子ブロックを持つか（リストのネスト判定等）
/// - `extra`: その他のフィールド（heading_1, paragraph 等の本体）
#[derive(Debug, Clone, Deserialize)]
pub struct BlockResponse {
    pub id: String,

    #[serde(rename = "type")]
    pub block_type: String,

    #[serde(default)]
    pub has_children: bool,

    #[serde(default)]
    pub archived: bool,

    /// type ごとの本体 (heading_1, paragraph, etc.) を含むその他のフィールド。
    /// serde の flatten でキャッチオールにする。
    #[serde(flatten)]
    pub extra: serde_json::Map<String, serde_json::Value>,
}

impl BlockResponse {
    /// このブロックの本体（block_type と同名のフィールド）を取得する。
    pub fn body(&self) -> Option<&serde_json::Value> {
        self.extra.get(&self.block_type)
    }

    /// このブロックの rich_text を取得する（該当する型のみ）。
    pub fn rich_text(&self) -> Vec<RichText> {
        let body = match self.body() {
            Some(b) => b,
            None => return Vec::new(),
        };
        body.get("rich_text")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| serde_json::from_value::<RichText>(v.clone()).ok())
                    .collect()
            })
            .unwrap_or_default()
    }

    /// 全 rich_text の plain_text を結合した文字列を返す。
    pub fn plain_text(&self) -> String {
        self.rich_text()
            .iter()
            .map(|rt| rt.plain_text.as_str())
            .collect::<String>()
    }
}

/// Notion の rich_text の最小型。
///
/// 構築側 (markdown → blocks) と読み取り側 (blocks → markdown) で共有する。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RichText {
    #[serde(rename = "type", default = "default_type_text")]
    pub type_: String,

    pub text: TextContent,

    #[serde(default)]
    pub annotations: Annotations,

    /// 読み取り時に使う、すべての装飾を平らにしたテキスト。
    /// 構築時は省略可。
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub plain_text: String,

    /// リンク先 URL（rich_text 内のリンク）。
    /// 構築時にリンクを埋め込んだ場合に使用。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub href: Option<String>,
}

fn default_type_text() -> String {
    "text".into()
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TextContent {
    pub content: String,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub link: Option<TextLink>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TextLink {
    pub url: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Annotations {
    #[serde(default)]
    pub bold: bool,
    #[serde(default)]
    pub italic: bool,
    #[serde(default)]
    pub strikethrough: bool,
    #[serde(default)]
    pub underline: bool,
    #[serde(default)]
    pub code: bool,
    #[serde(default = "default_color")]
    pub color: String,
}

fn default_color() -> String {
    "default".into()
}

// #[derive(Default)] を使うと color が "" になってしまい Notion API が 400 を返す。
// color だけ "default" を設定するために手動実装する。
impl Default for Annotations {
    fn default() -> Self {
        Self {
            bold: false,
            italic: false,
            strikethrough: false,
            underline: false,
            code: false,
            color: "default".into(),
        }
    }
}

impl RichText {
    /// プレーンテキストの rich_text を作成する。
    pub fn plain(content: impl Into<String>) -> Self {
        let content = content.into();
        Self {
            type_: "text".into(),
            text: TextContent {
                content: content.clone(),
                link: None,
            },
            annotations: Annotations::default(),
            plain_text: content,
            href: None,
        }
    }

    /// リンク付き rich_text を作成する。
    pub fn link(content: impl Into<String>, url: impl Into<String>) -> Self {
        let content = content.into();
        let url = url.into();
        Self {
            type_: "text".into(),
            text: TextContent {
                content: content.clone(),
                link: Some(TextLink { url: url.clone() }),
            },
            annotations: Annotations::default(),
            plain_text: content,
            href: Some(url),
        }
    }
}

/// データベースクエリのレスポンス。
#[derive(Debug, Deserialize)]
pub struct QueryDatabaseResponse {
    pub results: Vec<PageObject>,
    #[serde(default)]
    pub has_more: bool,
    #[serde(default)]
    pub next_cursor: Option<String>,
}

/// ページオブジェクト（DB クエリ / ページ作成のレスポンス）。
#[derive(Debug, Deserialize)]
pub struct PageObject {
    pub id: String,
    pub url: String,
    #[serde(default)]
    pub archived: bool,
    #[serde(default)]
    pub properties: serde_json::Map<String, serde_json::Value>,
}

impl PageObject {
    /// 指定したプロパティ名の title 配列から plain_text を結合して返す。
    ///
    /// Notion のレスポンス構造:
    /// ```json
    /// "properties": {
    ///   "名前": {
    ///     "type": "title",
    ///     "title": [{ "plain_text": "2026/05", ... }]
    ///   }
    /// }
    /// ```
    pub fn title_text(&self, property_name: &str) -> Option<String> {
        let prop = self.properties.get(property_name)?;
        let arr = prop.get("title")?.as_array()?;
        let text: String = arr
            .iter()
            .filter_map(|v| v.get("plain_text").and_then(|t| t.as_str()))
            .collect();
        if text.is_empty() { None } else { Some(text) }
    }
}

/// ブロック children 取得レスポンス。
#[derive(Debug, Deserialize)]
pub struct ListBlockChildrenResponse {
    pub results: Vec<BlockResponse>,
    #[serde(default)]
    pub has_more: bool,
    #[serde(default)]
    pub next_cursor: Option<String>,
}

/// データベース取得レスポンス（スキーマ取得用）。
/// title プロパティ名の自動検出に使う。
#[derive(Debug, Deserialize)]
pub struct DatabaseObject {
    pub id: String,

    /// プロパティ名 → プロパティ定義 のマップ。
    /// title プロパティを持つキーが、そのデータベースの「タイトルプロパティ名」。
    pub properties: std::collections::HashMap<String, DatabaseProperty>,
}

#[derive(Debug, Deserialize)]
pub struct DatabaseProperty {
    #[serde(rename = "type")]
    pub type_: String,
}

impl DatabaseObject {
    /// type=title のプロパティ名を返す。
    /// Notion DB は必ず 1 つの title プロパティを持つので、見つからなければ異常。
    pub fn find_title_property(&self) -> Option<&str> {
        self.properties
            .iter()
            .find(|(_, p)| p.type_ == "title")
            .map(|(k, _)| k.as_str())
    }
}
