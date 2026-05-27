//! Notion API から取得したブロック列を Markdown 文字列に戻す。
//!
//! 用途: `get_recent_diary` で過去日記を MCP Client に返却する際の表示。
//!
//! 注意: 双方向で完全な対称性は保証しない。
//! Notion は表現力豊かで、Markdown に戻せない情報（色、特殊な block 種別等）は
//! ベストエフォートで丸めるか、無視する。

use crate::notion::types::{BlockResponse, RichText};

/// ブロック列を Markdown に変換する。
pub fn blocks_to_markdown(blocks: &[BlockResponse]) -> String {
    let mut buf = String::new();
    render_blocks(blocks, 0, &mut buf);
    buf
}

fn render_blocks(blocks: &[BlockResponse], indent: usize, out: &mut String) {
    let mut numbered_counter: u32 = 0;
    let mut prev_was_numbered = false;

    for block in blocks {
        // 番号付きリストの連番を維持する。直前が numbered でなければカウンタリセット。
        if block.block_type == "numbered_list_item" {
            if !prev_was_numbered {
                numbered_counter = 0;
            }
            numbered_counter += 1;
            prev_was_numbered = true;
        } else {
            prev_was_numbered = false;
            numbered_counter = 0;
        }

        render_block(block, indent, numbered_counter, out);
    }
}

fn render_block(block: &BlockResponse, indent: usize, numbered_index: u32, out: &mut String) {
    let pad = "  ".repeat(indent);

    match block.block_type.as_str() {
        "heading_1" => {
            out.push_str(&pad);
            out.push_str("# ");
            out.push_str(&render_rich_text(&block.rich_text()));
            out.push_str("\n\n");
        }
        "heading_2" => {
            out.push_str(&pad);
            out.push_str("## ");
            out.push_str(&render_rich_text(&block.rich_text()));
            out.push_str("\n\n");
        }
        "heading_3" => {
            out.push_str(&pad);
            out.push_str("### ");
            out.push_str(&render_rich_text(&block.rich_text()));
            out.push_str("\n\n");
        }
        "paragraph" => {
            let text = render_rich_text(&block.rich_text());
            if text.is_empty() {
                // 空段落は空行 1 つに丸める
                out.push('\n');
            } else {
                out.push_str(&pad);
                out.push_str(&text);
                out.push_str("\n\n");
            }
        }
        "bulleted_list_item" => {
            out.push_str(&pad);
            out.push_str("- ");
            out.push_str(&render_rich_text(&block.rich_text()));
            out.push('\n');
            // 子要素は serde_json::Value で内包されているが、
            // この段階では子要素は別途 has_children=true でしか分からない。
            // get_recent_diary 側で必要なら children 取得を再帰的に行うが、
            // ここではあくまで MVP として親レベルのみ対応する。
        }
        "numbered_list_item" => {
            out.push_str(&pad);
            out.push_str(&format!("{numbered_index}. "));
            out.push_str(&render_rich_text(&block.rich_text()));
            out.push('\n');
        }
        "to_do" => {
            let checked = block
                .body()
                .and_then(|b| b.get("checked"))
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            let mark = if checked { "x" } else { " " };
            out.push_str(&pad);
            out.push_str(&format!("- [{mark}] "));
            out.push_str(&render_rich_text(&block.rich_text()));
            out.push('\n');
        }
        "quote" => {
            // 引用内の改行は各行の先頭に "> " を付与する。
            let text = render_rich_text(&block.rich_text());
            for line in text.split('\n') {
                out.push_str(&pad);
                out.push_str("> ");
                out.push_str(line);
                out.push('\n');
            }
            out.push('\n');
        }
        "code" => {
            let language = block
                .body()
                .and_then(|b| b.get("language"))
                .and_then(|v| v.as_str())
                .unwrap_or("plain text");
            // "plain text" は Markdown では空文字に
            let lang = if language == "plain text" {
                ""
            } else {
                language
            };
            let code: String = block
                .rich_text()
                .iter()
                .map(|rt| rt.plain_text.as_str())
                .collect();
            out.push_str(&pad);
            out.push_str(&format!("```{lang}\n"));
            out.push_str(&code);
            if !code.ends_with('\n') {
                out.push('\n');
            }
            out.push_str(&pad);
            out.push_str("```\n\n");
        }
        "divider" => {
            out.push_str(&pad);
            out.push_str("---\n\n");
        }
        // サポート外のブロック種別はテキストだけ出して、種別を脚注的にコメント
        other => {
            let text = render_rich_text(&block.rich_text());
            if !text.is_empty() {
                out.push_str(&pad);
                out.push_str(&text);
                out.push_str(&format!("  <!-- {other} -->\n\n"));
            }
        }
    }
}

/// rich_text 配列を Markdown 装飾付き文字列に変換する。
fn render_rich_text(rt: &[RichText]) -> String {
    let mut out = String::new();
    for r in rt {
        let mut s = if r.plain_text.is_empty() {
            r.text.content.clone()
        } else {
            r.plain_text.clone()
        };

        // annotations 適用（内側から外側へ）
        if r.annotations.code {
            s = format!("`{s}`");
        }
        if r.annotations.bold {
            s = format!("**{s}**");
        }
        if r.annotations.italic {
            s = format!("*{s}*");
        }
        if r.annotations.strikethrough {
            s = format!("~~{s}~~");
        }

        // リンク
        if let Some(url) = r.text.link.as_ref().map(|l| &l.url).or(r.href.as_ref()) {
            s = format!("[{s}]({url})");
        }

        out.push_str(&s);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn block(json_value: serde_json::Value) -> BlockResponse {
        serde_json::from_value(json_value).unwrap()
    }

    #[test]
    fn 見出し1() {
        let b = block(json!({
            "id": "x",
            "type": "heading_1",
            "heading_1": {
                "rich_text": [{
                    "type": "text",
                    "text": { "content": "Title" },
                    "annotations": {},
                    "plain_text": "Title"
                }]
            }
        }));
        let md = blocks_to_markdown(&[b]);
        assert!(md.starts_with("# Title"));
    }

    #[test]
    fn 太字段落() {
        let b = block(json!({
            "id": "x",
            "type": "paragraph",
            "paragraph": {
                "rich_text": [{
                    "type": "text",
                    "text": { "content": "hello" },
                    "annotations": { "bold": true },
                    "plain_text": "hello"
                }]
            }
        }));
        let md = blocks_to_markdown(&[b]);
        assert!(md.contains("**hello**"));
    }

    #[test]
    fn チェックボックス() {
        let b = block(json!({
            "id": "x",
            "type": "to_do",
            "to_do": {
                "rich_text": [{
                    "type": "text",
                    "text": { "content": "task" },
                    "annotations": {},
                    "plain_text": "task"
                }],
                "checked": true
            }
        }));
        let md = blocks_to_markdown(&[b]);
        assert!(md.contains("- [x] task"));
    }
}
