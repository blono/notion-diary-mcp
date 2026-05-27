//! `get_recent_diary` ツールの中核ロジック。
//!
//! 動作:
//! 1. 今日 (JST) を起点に、days 日分の日付範囲を計算
//! 2. 範囲が複数月にまたがる場合は、関係する月ページをすべて取得
//! 3. 各月ページのブロックを走査して、範囲内の日付の見出し+本文を抽出
//! 4. Markdown として連結して返す

use crate::diary::heading::find_heading_index;
use crate::diary::month_page::MonthPageResolver;
use crate::error::{AppError, AppResult};
use crate::markdown::blocks_to_markdown;
use crate::notion::NotionClient;
use crate::notion::types::BlockResponse;
use crate::time_util::{diary_heading_text, month_page_title, months_in_range, today_jst};
use chrono::{Datelike, NaiveDate};
use std::collections::HashMap;
use tracing::debug;

/// 直近 N 日分の日記の取得結果。
pub struct RecentDiary {
    pub markdown: String,
    pub from: NaiveDate,
    pub to: NaiveDate,
    pub diary_count: usize,
}

/// `days` 日分（今日を含む）の日記を取得する。
///
/// 範囲: today - (days - 1) .. today（両端含む、JST 基準）
pub async fn get_recent_diary(
    client: &NotionClient,
    resolver: &MonthPageResolver,
    days: u32,
) -> AppResult<RecentDiary> {
    if days == 0 {
        return Err(AppError::InvalidInput(
            "days は 1 以上を指定してください".into(),
        ));
    }
    if days > 31 {
        return Err(AppError::InvalidInput(
            "days は 31 以下を指定してください".into(),
        ));
    }

    let to = today_jst();
    let from = to - chrono::Duration::days((days - 1) as i64);

    // 関係する月ページを集める（古い順）
    let months = months_in_range(from, to);

    // 各月の月ページの blocks を取得（なければスキップ）
    let title_prop = resolver.title_property_name().await?.to_string();
    let database_id = resolver.database_id().to_string();
    let mut month_blocks: HashMap<(i32, u32), Vec<BlockResponse>> = HashMap::new();
    for &(y, m) in &months {
        // 月ページタイトルを生成（タイトルは "YYYY/MM" なので日にちは何でも良い）
        let date = NaiveDate::from_ymd_opt(y, m, 1).unwrap();
        let title = month_page_title(date);
        let page = client
            .find_page_by_title(&database_id, &title_prop, &title)
            .await?;
        if let Some(p) = page {
            let blocks = client.list_all_block_children(&p.id).await?;
            month_blocks.insert((y, m), blocks);
        }
    }

    // 範囲内の各日付について、見出し以降のブロックを抽出
    let mut diary_count = 0usize;
    let mut buf = String::new();

    let mut cursor = from;
    while cursor <= to {
        let key = (cursor.year(), cursor.month());
        if let Some(blocks_for_month) = month_blocks.get(&key) {
            let heading_text = diary_heading_text(cursor);
            if let Some(idx) = find_heading_index(blocks_for_month, &heading_text) {
                // 次の heading_1 までを「その日のセクション」とする
                let section_end = blocks_for_month[idx + 1..]
                    .iter()
                    .position(|b| b.block_type == "heading_1")
                    .map(|p| idx + 1 + p)
                    .unwrap_or(blocks_for_month.len());

                // 見出しブロックを含めて Markdown 化
                let section = &blocks_for_month[idx..section_end];
                buf.push_str(&blocks_to_markdown(section));
                buf.push('\n');
                diary_count += 1;
                debug!(date = %cursor, "日記を取得しました");
            }
        }
        cursor = match cursor.succ_opt() {
            Some(d) => d,
            None => break, // 9999/12/31 超え等は実用上発生しない
        };
    }

    Ok(RecentDiary {
        markdown: buf,
        from,
        to,
        diary_count,
    })
}
