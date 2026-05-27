//! 月ページ（例: "2026/05"）を解決・作成するロジック。
//!
//! Notion DB「日記」の各 row が月ページ。
//! - タイトル一致で検索
//! - なければ新規作成
//! - DB のタイトルプロパティ名は OnceCell で最初の 1 回だけ取得してキャッシュ

use crate::error::AppResult;
use crate::notion::NotionClient;
use crate::notion::types::PageObject;
use chrono::NaiveDate;
use tokio::sync::OnceCell;
use tracing::info;

/// 月ページ解決サービス。
/// title プロパティ名のキャッシュを保持する。
pub struct MonthPageResolver {
    client: NotionClient,
    database_id: String,
    /// DB の title プロパティ名（例: "Name", "名前", "タイトル" 等）。
    /// 最初に DB スキーマを取得して特定し、以降はキャッシュを使う。
    title_property: OnceCell<String>,
}

impl MonthPageResolver {
    pub fn new(client: NotionClient, database_id: String) -> Self {
        Self {
            client,
            database_id,
            title_property: OnceCell::new(),
        }
    }

    /// データベース ID を返す（read 側でも使うため公開）。
    pub fn database_id(&self) -> &str {
        &self.database_id
    }

    /// タイトルプロパティ名を取得する。初回のみ DB スキーマを取得する。
    pub async fn title_property_name(&self) -> AppResult<&str> {
        let name = self
            .title_property
            .get_or_try_init(|| async {
                let db = self.client.get_database(&self.database_id).await?;
                let title = db.find_title_property().ok_or_else(|| {
                    crate::error::AppError::NotionApi(
                        "DB にタイトルプロパティ (type=title) が見つかりません。\
                         Notion DB の構造を確認してください。"
                            .into(),
                    )
                })?;
                info!(title_property = %title, "タイトルプロパティを検出してキャッシュしました");
                Ok::<String, crate::error::AppError>(title.to_string())
            })
            .await?;
        Ok(name.as_str())
    }

    /// 指定日付に対応する月ページを取得する（なければ作成）。
    pub async fn ensure(&self, date: NaiveDate) -> AppResult<EnsuredMonthPage> {
        let title = crate::time_util::month_page_title(date);
        let title_prop = self.title_property_name().await?.to_string();

        if let Some(p) = self
            .client
            .find_page_by_title(&self.database_id, &title_prop, &title)
            .await?
        {
            return Ok(EnsuredMonthPage {
                page: p,
                created: false,
            });
        }

        info!(month = %title, "月ページが存在しないため新規作成します");
        let p = self
            .client
            .create_page_in_database(&self.database_id, &title_prop, &title)
            .await?;
        Ok(EnsuredMonthPage {
            page: p,
            created: true,
        })
    }
}

/// 月ページの取得結果。created で「今回作成したか」が分かる。
pub struct EnsuredMonthPage {
    pub page: PageObject,
    pub created: bool,
}
