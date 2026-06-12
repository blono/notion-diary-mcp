//! `save_diary` ツールの中核ビジネスロジック。
//!
//! 仕様:
//! - 未来日付: 拒否
//! - 過去日上限超え: 拒否（max_past_days=0 で無制限）
//! - 月ページが無い → 自動作成
//! - 日見出しが無い → 見出し + 本文を追記
//! - 日見出しが有り、かつ「今日」 → 日のセクションを置き換え
//! - 日見出しが有り、かつ「過去日」 → 拒否

use crate::config::Config;
use crate::diary::heading::{collect_section_block_ids, find_heading_index};
use crate::diary::month_page::MonthPageResolver;
use crate::error::{AppError, AppResult};
use crate::markdown::markdown_to_blocks;
use crate::notion::NotionClient;
use crate::time_util::{diary_heading_text, is_today_jst, parse_date, validate_writable_date};
use serde_json::{Value, json};
use tracing::{info, warn};

/// save_diary の実行結果サマリ。
pub struct SaveResult {
    pub outcome: SaveDiaryOutcome,
    pub page_url: String,
    pub date: String,
    pub month: String,
    pub month_page_created: bool,
}

/// save_diary が取った具体的な操作。MCP Client / 人間に「何が起きたか」を伝える。
pub enum SaveDiaryOutcome {
    /// 新規追記（見出し作成 + 本文）
    Appended,
    /// 今日の日記を置き換えた（見出しは維持、本文を差し替え）
    Replaced { deleted_block_count: usize },
}

impl SaveDiaryOutcome {
    pub fn as_str(&self) -> &'static str {
        match self {
            SaveDiaryOutcome::Appended => "appended",
            SaveDiaryOutcome::Replaced { .. } => "replaced",
        }
    }
}

/// 日記を保存する。
///
/// 引数:
/// - `date_str`: "YYYY-MM-DD" 形式
/// - `content`: Markdown 本文（日付見出しは含めない）
pub async fn save_diary(
    config: &Config,
    client: &NotionClient,
    resolver: &MonthPageResolver,
    date_str: &str,
    content: &str,
) -> AppResult<SaveResult> {
    // 1. 入力バリデーション ----------------------------------------------------
    let date = parse_date(date_str)?;
    validate_writable_date(date, config.max_past_days)?;

    let trimmed = content.trim();
    if trimmed.is_empty() {
        return Err(AppError::InvalidInput(
            "content が空です。日記本文を指定してください。".into(),
        ));
    }
    if trimmed.len() > 10_000 {
        return Err(AppError::InvalidInput(
            "content が長すぎます（10 KB 以内にしてください）。".into(),
        ));
    }

    // 2. 月ページの解決（なければ作成） -----------------------------------------
    let month = crate::time_util::month_page_title(date);
    let ensured = resolver.ensure(date).await?;
    let month_page_id = ensured.page.id.clone();
    let month_page_url = ensured.page.url.clone();

    // 3. 既存ブロックを取得して、その日の見出しがあるか確認 --------------------
    let blocks = client.list_all_block_children(&month_page_id).await?;
    let heading_text = diary_heading_text(date);
    let existing_idx = find_heading_index(&blocks, &heading_text);
    let is_today = is_today_jst(date);

    // 4. 本文 markdown をブロック JSON に変換 ----------------------------------
    let content_blocks: Vec<Value> = markdown_to_blocks(trimmed);

    // 5. ディスパッチ ----------------------------------------------------------
    let outcome = match existing_idx {
        Some(idx) => {
            if !is_today {
                // 過去日 + 既存 → 拒否
                return Err(AppError::PastDateAlreadyExists {
                    date: date_str.to_string(),
                });
            }
            replace_today_section(client, &month_page_id, &blocks, idx, content_blocks).await?
        }
        None => {
            append_new_day(
                client,
                &month_page_id,
                &blocks,
                &heading_text,
                content_blocks,
            )
            .await?
        }
    };

    info!(
        date = %date_str,
        month = %month,
        outcome = outcome.as_str(),
        month_page_created = ensured.created,
        "save_diary 完了"
    );

    Ok(SaveResult {
        outcome,
        page_url: month_page_url,
        date: date_str.to_string(),
        month,
        month_page_created: ensured.created,
    })
}

/// 新規日: 見出し + 本文を月ページ末尾に append する。
///
/// 日記と日記の間に余計な空行を作らないため、月ページ末尾が空段落で
/// 終わっている場合は、その空段落を削除してから見出しを追記する。
async fn append_new_day(
    client: &NotionClient,
    month_page_id: &str,
    blocks: &[crate::notion::types::BlockResponse],
    heading_text: &str,
    content_blocks: Vec<Value>,
) -> AppResult<SaveDiaryOutcome> {
    // 末尾が空段落なら削除する（前の日記との間の余計な空行をなくす）。
    // Notion 上ではアーカイブ = ゴミ箱送りなので復旧可能。
    if let Some(last) = blocks.last() {
        if is_empty_paragraph(last) {
            client.delete_block(&last.id).await?;
        }
    }

    let heading_block = json!({
        "object": "block",
        "type": "heading_1",
        "heading_1": {
            "rich_text": [{
                "type": "text",
                "text": { "content": heading_text }
            }]
        }
    });

    let mut all = Vec::with_capacity(content_blocks.len() + 1);
    all.push(heading_block);
    all.extend(content_blocks);

    client
        .append_block_children(month_page_id, all, None)
        .await?;

    Ok(SaveDiaryOutcome::Appended)
}

/// ブロックが「空の段落ブロック」かどうかを判定する。
/// 空の段落ブロック = block_type が "paragraph" かつ rich_text が空。
fn is_empty_paragraph(block: &crate::notion::types::BlockResponse) -> bool {
    block.block_type == "paragraph" && block.rich_text().is_empty()
}

/// 今日の日記を差し替える: 既存セクションを削除 → 見出し直後に新本文を挿入。
async fn replace_today_section(
    client: &crate::notion::NotionClient,
    month_page_id: &str,
    blocks: &[crate::notion::types::BlockResponse],
    heading_idx: usize,
    content_blocks: Vec<Value>,
) -> AppResult<SaveDiaryOutcome> {
    let heading_block_id = blocks[heading_idx].id.clone();
    let to_delete = collect_section_block_ids(blocks, heading_idx);

    warn!(
        heading_block_id = %heading_block_id,
        deletion_count = to_delete.len(),
        "今日の日記セクションを置き換えます（既存ブロックをアーカイブ）"
    );

    // 既存ブロックを削除（Notion 上ではアーカイブ = ゴミ箱送り、復旧可）
    for id in &to_delete {
        client.delete_block(id).await?;
    }

    // 月ページ直下の見出し直後に新ブロック群を挿入
    // （parent は月ページ ID、after は見出しブロック ID）
    client
        .append_block_children(month_page_id, content_blocks, Some(&heading_block_id))
        .await?;

    Ok(SaveDiaryOutcome::Replaced {
        deleted_block_count: to_delete.len(),
    })
}
