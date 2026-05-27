//! 月ページ内の日付見出し ("YYYY/MM/DD(曜)") を検出するロジック。
//!
//! 日記の構造:
//! ```
//! [heading_1: 2026/05/01(金)]   <- 日見出し
//! [paragraph: ...]              <- その日の本文
//! [paragraph: ...]
//! [heading_1: 2026/05/02(土)]   <- 次の日の見出し
//! [paragraph: ...]
//! ```
//!
//! 「日のセクション」は、ある日見出しの直後から、次の日見出し直前
//! （または月ページ末尾）までの範囲を指す。

use crate::notion::types::BlockResponse;

/// 日付見出しテキストにマッチする heading_1 ブロックの位置を探す。
///
/// マッチ条件: `block_type == "heading_1"` かつ rich_text のプレーンテキストが
/// 期待値と完全一致（前後空白は trim）する。
pub fn find_heading_index(blocks: &[BlockResponse], heading_text: &str) -> Option<usize> {
    let target = heading_text.trim();
    blocks
        .iter()
        .position(|b| b.block_type == "heading_1" && b.plain_text().trim() == target)
}

/// 指定 index にある日見出しの「日のセクション」のブロック ID 一覧を返す。
///
/// 戻り値: 日見出し直後から、次の heading_1 直前（または末尾）までの
/// ブロック ID の Vec。
/// （見出しブロック自体は含まない）
pub fn collect_section_block_ids(blocks: &[BlockResponse], heading_idx: usize) -> Vec<String> {
    let start = heading_idx + 1;
    let end = blocks[start..]
        .iter()
        .position(|b| b.block_type == "heading_1")
        .map(|p| start + p)
        .unwrap_or(blocks.len());
    blocks[start..end].iter().map(|b| b.id.clone()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn make_block(id: &str, type_: &str, text: &str) -> BlockResponse {
        let body = json!({
            "rich_text": [{
                "type": "text",
                "text": { "content": text },
                "annotations": {},
                "plain_text": text
            }]
        });
        let v = json!({
            "id": id,
            "type": type_,
            type_: body
        });
        serde_json::from_value(v).unwrap()
    }

    #[test]
    fn 見出し検出_正常系() {
        let blocks = vec![
            make_block("a", "heading_1", "2026/05/01(金)"),
            make_block("b", "paragraph", "本文1"),
            make_block("c", "heading_1", "2026/05/02(土)"),
            make_block("d", "paragraph", "本文2"),
        ];
        assert_eq!(find_heading_index(&blocks, "2026/05/01(金)"), Some(0));
        assert_eq!(find_heading_index(&blocks, "2026/05/02(土)"), Some(2));
        assert_eq!(find_heading_index(&blocks, "2026/05/03(日)"), None);
    }

    #[test]
    fn セクション収集() {
        let blocks = vec![
            make_block("a", "heading_1", "2026/05/01(金)"),
            make_block("b", "paragraph", "本文1"),
            make_block("c", "paragraph", "本文1の続き"),
            make_block("d", "heading_1", "2026/05/02(土)"),
            make_block("e", "paragraph", "本文2"),
        ];
        // 5/1 のセクション
        let ids = collect_section_block_ids(&blocks, 0);
        assert_eq!(ids, vec!["b".to_string(), "c".to_string()]);

        // 5/2 のセクション（末尾まで）
        let ids = collect_section_block_ids(&blocks, 3);
        assert_eq!(ids, vec!["e".to_string()]);
    }
}
