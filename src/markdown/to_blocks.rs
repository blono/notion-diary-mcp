//! Markdown 文字列を Notion API 用の block JSON 配列に変換する。
//!
//! 変換のポイント:
//! - pulldown-cmark のイベントストリームを再帰的に処理することで、
//!   入れ子のリスト等を素直に扱う。
//! - rich_text の annotations（太字、斜体、打ち消し、inline code, リンク）は
//!   状態スタックで管理する。
//! - チェックボックス (- [ ] / - [x]) は Notion の to_do block に変換する。
//! - 区切り線 (---) は Notion の divider block に変換する。

use crate::notion::types::RichText;
use pulldown_cmark::{CodeBlockKind, Event, HeadingLevel, Options, Parser, Tag, TagEnd};
use serde_json::{Value, json};

/// Markdown 文字列を Notion block JSON の配列に変換する。
pub fn markdown_to_blocks(md: &str) -> Vec<Value> {
    let mut opts = Options::empty();
    opts.insert(Options::ENABLE_STRIKETHROUGH);
    opts.insert(Options::ENABLE_TASKLISTS);

    let parser = Parser::new_ext(md, opts);
    // pulldown-cmark の Event<'a> は入力文字列のライフタイムに紐付くので、
    // into_static() で所有権付き Event<'static> に変換してから
    // peekable iterator を作る（再帰関数間で取り回しやすくするため）。
    let events: Vec<Event<'static>> = parser.map(|e| e.into_static()).collect();
    let mut iter = events.into_iter().peekable();
    let blocks = parse_blocks(&mut iter, None);
    blocks.into_iter().map(|b| b.into_json()).collect()
}

// =============================================================================
// 内部表現
// =============================================================================

/// 中間表現としての Notion ブロック。最後に JSON にシリアライズする。
#[derive(Debug)]
enum Block {
    Heading(HeadingLevel, Vec<RichText>),
    Paragraph(Vec<RichText>),
    BulletedListItem(Vec<RichText>, Vec<Block>),
    NumberedListItem(Vec<RichText>, Vec<Block>),
    /// `- [ ]` / `- [x]` のチェックボックス
    Todo {
        checked: bool,
        rich_text: Vec<RichText>,
        children: Vec<Block>,
    },
    Quote(Vec<RichText>),
    Code {
        language: String,
        code: String,
    },
    Divider,
}

impl Block {
    /// Notion API に送る JSON へ変換する。
    fn into_json(self) -> Value {
        match self {
            Block::Heading(level, rt) => {
                let key = match level {
                    HeadingLevel::H1 => "heading_1",
                    HeadingLevel::H2 => "heading_2",
                    HeadingLevel::H3 => "heading_3",
                    // H4 以降は Notion に無いので H3 に丸める
                    _ => "heading_3",
                };
                json!({
                    "object": "block",
                    "type": key,
                    key: { "rich_text": rt }
                })
            }
            Block::Paragraph(rt) => json!({
                "object": "block",
                "type": "paragraph",
                "paragraph": { "rich_text": rt }
            }),
            Block::BulletedListItem(rt, children) => {
                let mut body = json!({ "rich_text": rt });
                if !children.is_empty() {
                    body["children"] =
                        Value::Array(children.into_iter().map(|b| b.into_json()).collect());
                }
                json!({
                    "object": "block",
                    "type": "bulleted_list_item",
                    "bulleted_list_item": body
                })
            }
            Block::NumberedListItem(rt, children) => {
                let mut body = json!({ "rich_text": rt });
                if !children.is_empty() {
                    body["children"] =
                        Value::Array(children.into_iter().map(|b| b.into_json()).collect());
                }
                json!({
                    "object": "block",
                    "type": "numbered_list_item",
                    "numbered_list_item": body
                })
            }
            Block::Todo {
                checked,
                rich_text,
                children,
            } => {
                let mut body = json!({
                    "rich_text": rich_text,
                    "checked": checked,
                });
                if !children.is_empty() {
                    body["children"] =
                        Value::Array(children.into_iter().map(|b| b.into_json()).collect());
                }
                json!({
                    "object": "block",
                    "type": "to_do",
                    "to_do": body
                })
            }
            Block::Quote(rt) => json!({
                "object": "block",
                "type": "quote",
                "quote": { "rich_text": rt }
            }),
            Block::Code { language, code } => json!({
                "object": "block",
                "type": "code",
                "code": {
                    "rich_text": [RichText::plain(code)],
                    // Notion の code block は言語必須。未指定時は plain text。
                    "language": normalize_code_language(&language)
                }
            }),
            Block::Divider => json!({
                "object": "block",
                "type": "divider",
                "divider": {}
            }),
        }
    }
}

/// rich_text 構築用の状態。
#[derive(Default, Clone)]
struct InlineState {
    bold: bool,
    italic: bool,
    strikethrough: bool,
    code: bool,
    /// リンク先 URL（リンク中であれば Some）
    link: Option<String>,
}

impl InlineState {
    fn make_rich_text(&self, text: String) -> RichText {
        let mut rt = RichText::plain(text);
        rt.annotations.bold = self.bold;
        rt.annotations.italic = self.italic;
        rt.annotations.strikethrough = self.strikethrough;
        rt.annotations.code = self.code;
        if let Some(url) = &self.link {
            rt.text.link = Some(crate::notion::types::TextLink { url: url.clone() });
            rt.href = Some(url.clone());
        }
        rt
    }
}

// =============================================================================
// パース本体
// =============================================================================

type EventIter = std::iter::Peekable<std::vec::IntoIter<Event<'static>>>;

/// イベント列をブロック列に変換する。
///
/// `until` が指定されている場合、該当する `End` タグでパースを終了する
/// （リスト項目の中身などの再帰呼び出しに使用）。
fn parse_blocks(iter: &mut EventIter, until: Option<&TagEnd>) -> Vec<Block> {
    let mut blocks: Vec<Block> = Vec::new();

    while let Some(ev) = iter.peek() {
        // 終端タグに到達したら抜ける
        if let Some(end_tag) = until {
            if let Event::End(t) = ev {
                if tag_end_eq(t, end_tag) {
                    iter.next(); // consume end
                    return blocks;
                }
            }
        }

        let ev = iter.next().unwrap();
        match ev {
            Event::Start(tag) => match tag {
                Tag::Heading { level, .. } => {
                    let rt = parse_inline_until(iter, TagEnd::Heading(level));
                    blocks.push(Block::Heading(level, rt));
                }
                Tag::Paragraph => {
                    let rt = parse_inline_until(iter, TagEnd::Paragraph);
                    if !rt.is_empty() {
                        blocks.push(Block::Paragraph(rt));
                    }
                }
                Tag::List(start) => {
                    // List の中の各 Item を読む
                    let ordered = start.is_some();
                    parse_list_items(iter, ordered, &mut blocks);
                }
                Tag::Item => {
                    // 通常は List 経由で来るが、保険として直接 Item が来た場合も処理
                    parse_one_item(iter, false, &mut blocks);
                }
                Tag::BlockQuote(kind) => {
                    // BlockQuote 内のすべてのテキストを集めて、1 つの quote ブロックにまとめる。
                    // pulldown-cmark 0.12 では TagEnd::BlockQuote(Option<BlockQuoteKind>) なので
                    // 開始 Tag の kind をそのまま引き継いで until に渡す。
                    let inner = parse_blocks(iter, Some(&TagEnd::BlockQuote(kind)));
                    let merged = merge_blocks_to_rich_text(inner);
                    blocks.push(Block::Quote(merged));
                }
                Tag::CodeBlock(kind) => {
                    let language = match kind {
                        CodeBlockKind::Fenced(lang) => lang.to_string(),
                        CodeBlockKind::Indented => String::new(),
                    };
                    let mut code = String::new();
                    while let Some(ev) = iter.next() {
                        match ev {
                            Event::Text(t) => code.push_str(&t),
                            Event::End(TagEnd::CodeBlock) => break,
                            _ => {}
                        }
                    }
                    // 末尾の改行は Notion 上で見栄えが悪いので削除
                    while code.ends_with('\n') {
                        code.pop();
                    }
                    blocks.push(Block::Code { language, code });
                }
                // インライン用の Tag が単体で来るケース（普通は parse_inline_until で消費される）
                Tag::Emphasis
                | Tag::Strong
                | Tag::Strikethrough
                | Tag::Link { .. }
                | Tag::Image { .. } => {
                    // ブロックの外でインラインタグが来るケースは pulldown-cmark の仕様上ほぼ無い。
                    // 念のため対応する End まで読み飛ばす。
                    skip_until_matching_end(iter, &tag_end_for(&tag));
                }
                // サポート外のタグはまるごとスキップ
                _ => {
                    skip_until_matching_end(iter, &tag_end_for(&tag));
                }
            },
            Event::Rule => blocks.push(Block::Divider),
            Event::SoftBreak | Event::HardBreak => {
                // ブロック境界での break は無視（段落内であれば parse_inline_until で処理済）
            }
            Event::Text(_) | Event::Code(_) | Event::Html(_) | Event::InlineHtml(_) => {
                // ブロック境界の素のテキスト/HTML は無視（markdown としては不正に近い）
            }
            Event::FootnoteReference(_)
            | Event::TaskListMarker(_)
            | Event::InlineMath(_)
            | Event::DisplayMath(_) => {
                // 想定外のイベントはスキップ
            }
            Event::End(_) => {
                // ここまで来るのは想定外（until で処理されているはず）だが、フェイルセーフ
            }
        }
    }
    blocks
}

/// List 内の Item を順に処理して blocks に push する。
fn parse_list_items(iter: &mut EventIter, ordered: bool, blocks: &mut Vec<Block>) {
    while let Some(ev) = iter.peek() {
        match ev {
            Event::Start(Tag::Item) => {
                iter.next();
                parse_one_item(iter, ordered, blocks);
            }
            Event::End(TagEnd::List(_)) => {
                iter.next();
                return;
            }
            _ => {
                // 想定外のイベントはスキップ
                iter.next();
            }
        }
    }
}

/// 1 つの List Item を解釈する。
///
/// pulldown-cmark の List Item の中身は、以下の順で来る:
///   - tight list（項目間に空行なし）: 本文が `Text` 等のインラインで直接来る
///   - loose list（項目間に空行あり）: 本文が `Paragraph` で包まれて来る
///   - 入れ子: 本文インラインの後に `Start(List)` が続く
/// よって「インライン列を rich_text に集めつつ、ネストブロックの Start が来たら
/// children に回す」という方針で 1 ループにまとめて処理する。
/// TaskListMarker が先頭にあれば to_do block として扱う。
fn parse_one_item(iter: &mut EventIter, ordered: bool, blocks: &mut Vec<Block>) {
    // 先頭が TaskListMarker かチェック
    let mut is_todo = false;
    let mut todo_checked = false;
    if let Some(Event::TaskListMarker(checked)) = iter.peek() {
        is_todo = true;
        todo_checked = *checked;
        iter.next();
    }

    let mut rich_text: Vec<RichText> = Vec::new();
    let mut children: Vec<Block> = Vec::new();
    // インライン装飾の状態（太字・斜体・リンク等）
    let mut state = InlineState::default();
    let mut state_stack: Vec<InlineState> = Vec::new();

    while let Some(ev) = iter.next() {
        match ev {
            // Item の終了
            Event::End(TagEnd::Item) => break,

            // ---- 本文インライン（tight list はここに直接来る） ----
            Event::Text(t) => {
                if !t.is_empty() {
                    rich_text.push(state.make_rich_text(t.to_string()));
                }
            }
            Event::Code(t) => {
                let mut s = state.clone();
                s.code = true;
                rich_text.push(s.make_rich_text(t.to_string()));
            }
            Event::SoftBreak => {
                rich_text.push(state.make_rich_text(" ".into()));
            }
            Event::HardBreak => {
                rich_text.push(state.make_rich_text("\n".into()));
            }
            Event::Start(Tag::Strong) => {
                state_stack.push(state.clone());
                state.bold = true;
            }
            Event::Start(Tag::Emphasis) => {
                state_stack.push(state.clone());
                state.italic = true;
            }
            Event::Start(Tag::Strikethrough) => {
                state_stack.push(state.clone());
                state.strikethrough = true;
            }
            Event::Start(Tag::Link { dest_url, .. }) => {
                state_stack.push(state.clone());
                state.link = Some(dest_url.to_string());
            }
            Event::Start(Tag::Image { dest_url, .. }) => {
                state_stack.push(state.clone());
                state.link = Some(dest_url.to_string());
            }
            Event::End(TagEnd::Strong)
            | Event::End(TagEnd::Emphasis)
            | Event::End(TagEnd::Strikethrough)
            | Event::End(TagEnd::Link)
            | Event::End(TagEnd::Image) => {
                if let Some(prev) = state_stack.pop() {
                    state = prev;
                }
            }
            Event::Html(t) | Event::InlineHtml(t) => {
                rich_text.push(state.make_rich_text(t.to_string()));
            }

            // ---- loose list: 本文が Paragraph で包まれて来るケース ----
            Event::Start(Tag::Paragraph) => {
                let rt = parse_inline_until(iter, TagEnd::Paragraph);
                if rich_text.is_empty() {
                    // まだ本文未確定なら、この段落を本文 rich_text にする
                    rich_text = rt;
                } else if !rt.is_empty() {
                    // 2 つ目以降の段落は children の段落ブロックとして追加
                    children.push(Block::Paragraph(rt));
                }
            }

            // ---- ネストしたリスト: children に回す ----
            Event::Start(Tag::List(start)) => {
                let nested_ordered = start.is_some();
                parse_list_items(iter, nested_ordered, &mut children);
            }

            // ---- その他のネストブロック（コードブロック等） ----
            Event::Start(Tag::CodeBlock(kind)) => {
                let language = match kind {
                    CodeBlockKind::Fenced(lang) => lang.to_string(),
                    CodeBlockKind::Indented => String::new(),
                };
                let mut code = String::new();
                while let Some(ev) = iter.next() {
                    match ev {
                        Event::Text(t) => code.push_str(&t),
                        Event::End(TagEnd::CodeBlock) => break,
                        _ => {}
                    }
                }
                while code.ends_with('\n') {
                    code.pop();
                }
                children.push(Block::Code { language, code });
            }

            // 想定外の Start は対応する End まで読み飛ばす（整合性維持）
            Event::Start(other) => {
                skip_until_matching_end(iter, &tag_end_for(&other));
            }

            // それ以外（想定外の End 等）は無視
            _ => {}
        }
    }

    // 空アイテムのガード:
    // markdown 末尾の余分な改行・空行などで pulldown-cmark が中身のない Item を
    // 生成することがある。rich_text も children も空なら弾く（空のリスト項目が
    // Notion に作られるのを防ぐ）。to_do は空チェックの可能性があるため対象外。
    if !is_todo && rich_text.is_empty() && children.is_empty() {
        return;
    }

    if is_todo {
        blocks.push(Block::Todo {
            checked: todo_checked,
            rich_text,
            children,
        });
    } else if ordered {
        blocks.push(Block::NumberedListItem(rich_text, children));
    } else {
        blocks.push(Block::BulletedListItem(rich_text, children));
    }
}

/// 指定された End タグまで、インライン要素を rich_text に蓄積する。
fn parse_inline_until(iter: &mut EventIter, end: TagEnd) -> Vec<RichText> {
    let mut rich_text: Vec<RichText> = Vec::new();
    let mut state = InlineState::default();
    let mut state_stack: Vec<InlineState> = Vec::new();

    while let Some(ev) = iter.next() {
        match ev {
            Event::End(t) if tag_end_eq(&t, &end) => break,
            Event::Text(t) => {
                if !t.is_empty() {
                    rich_text.push(state.make_rich_text(t.to_string()));
                }
            }
            Event::Code(t) => {
                let mut s = state.clone();
                s.code = true;
                rich_text.push(s.make_rich_text(t.to_string()));
            }
            Event::SoftBreak => {
                // markdown の改行 (\n) は Notion の段落内では空白扱いが自然
                rich_text.push(state.make_rich_text(" ".into()));
            }
            Event::HardBreak => {
                rich_text.push(state.make_rich_text("\n".into()));
            }
            Event::Start(Tag::Strong) => {
                state_stack.push(state.clone());
                state.bold = true;
            }
            Event::Start(Tag::Emphasis) => {
                state_stack.push(state.clone());
                state.italic = true;
            }
            Event::Start(Tag::Strikethrough) => {
                state_stack.push(state.clone());
                state.strikethrough = true;
            }
            Event::Start(Tag::Link { dest_url, .. }) => {
                state_stack.push(state.clone());
                state.link = Some(dest_url.to_string());
            }
            Event::End(TagEnd::Strong)
            | Event::End(TagEnd::Emphasis)
            | Event::End(TagEnd::Strikethrough)
            | Event::End(TagEnd::Link) => {
                if let Some(prev) = state_stack.pop() {
                    state = prev;
                }
            }
            // 画像はリンクとして扱う（link annotation のみ、ファイル化しない）
            Event::Start(Tag::Image { dest_url, .. }) => {
                state_stack.push(state.clone());
                state.link = Some(dest_url.to_string());
            }
            Event::End(TagEnd::Image) => {
                if let Some(prev) = state_stack.pop() {
                    state = prev;
                }
            }
            // インライン HTML は素通し（テキストとして扱う）
            Event::Html(t) | Event::InlineHtml(t) => {
                rich_text.push(state.make_rich_text(t.to_string()));
            }
            // 想定外の Start/End はネストの整合性のために対応する End まで読み飛ばす
            Event::Start(t) => {
                skip_until_matching_end(iter, &tag_end_for(&t));
            }
            _ => {}
        }
    }

    rich_text
}

/// 指定された End タグまでイベントを読み飛ばす。
fn skip_until_matching_end(iter: &mut EventIter, end: &TagEnd) {
    let mut depth = 1usize;
    while let Some(ev) = iter.next() {
        match ev {
            Event::Start(t) => {
                if tag_end_for(&t) == *end {
                    depth += 1;
                }
            }
            Event::End(t) => {
                if tag_end_eq(&t, end) {
                    depth -= 1;
                    if depth == 0 {
                        return;
                    }
                }
            }
            _ => {}
        }
    }
}

/// Tag に対応する TagEnd を生成する。
fn tag_end_for(tag: &Tag) -> TagEnd {
    match tag {
        Tag::Paragraph => TagEnd::Paragraph,
        Tag::Heading { level, .. } => TagEnd::Heading(*level),
        Tag::BlockQuote(kind) => TagEnd::BlockQuote(*kind),
        Tag::CodeBlock(_) => TagEnd::CodeBlock,
        Tag::List(start) => TagEnd::List(start.is_some()),
        Tag::Item => TagEnd::Item,
        Tag::Emphasis => TagEnd::Emphasis,
        Tag::Strong => TagEnd::Strong,
        Tag::Strikethrough => TagEnd::Strikethrough,
        Tag::Link { .. } => TagEnd::Link,
        Tag::Image { .. } => TagEnd::Image,
        Tag::HtmlBlock => TagEnd::HtmlBlock,
        Tag::Table(_) => TagEnd::Table,
        Tag::TableHead => TagEnd::TableHead,
        Tag::TableRow => TagEnd::TableRow,
        Tag::TableCell => TagEnd::TableCell,
        Tag::FootnoteDefinition(_) => TagEnd::FootnoteDefinition,
        Tag::MetadataBlock(kind) => TagEnd::MetadataBlock(*kind),
        Tag::DefinitionList => TagEnd::DefinitionList,
        Tag::DefinitionListTitle => TagEnd::DefinitionListTitle,
        Tag::DefinitionListDefinition => TagEnd::DefinitionListDefinition,
        Tag::Superscript => TagEnd::Superscript,
        Tag::Subscript => TagEnd::Subscript,
    }
}

/// TagEnd の同一性を判定する。
fn tag_end_eq(a: &TagEnd, b: &TagEnd) -> bool {
    std::mem::discriminant(a) == std::mem::discriminant(b) && format!("{a:?}") == format!("{b:?}")
}

/// 複数ブロックを 1 つの rich_text 列にマージする（引用ブロックの内部用）。
fn merge_blocks_to_rich_text(blocks: Vec<Block>) -> Vec<RichText> {
    let mut out: Vec<RichText> = Vec::new();
    for (i, b) in blocks.into_iter().enumerate() {
        let rt = match b {
            Block::Paragraph(rt) => rt,
            Block::Heading(_, rt) => rt,
            Block::BulletedListItem(rt, _) | Block::NumberedListItem(rt, _) | Block::Quote(rt) => {
                rt
            }
            Block::Todo { rich_text, .. } => rich_text,
            Block::Code { code, .. } => vec![RichText::plain(code)],
            Block::Divider => vec![RichText::plain("---")],
        };
        if i > 0 {
            // &str を直接渡す（.into() は reqwest/bytes の From 実装と競合して型推論が壊れる）
            out.push(RichText::plain("\n"));
        }
        out.extend(rt);
    }
    out
}

/// pulldown-cmark の言語指定（空文字含む）を Notion の code block で使える言語名に正規化する。
///
/// Notion がサポートしない言語は "plain text" にフォールバックする。
fn normalize_code_language(lang: &str) -> &'static str {
    match lang.trim().to_lowercase().as_str() {
        "" | "txt" | "text" | "plain" => "plain text",
        "rs" | "rust" => "rust",
        "go" | "golang" => "go",
        "js" | "javascript" => "javascript",
        "ts" | "typescript" => "typescript",
        "tsx" => "typescript",
        "jsx" => "javascript",
        "py" | "python" => "python",
        "java" => "java",
        "kt" | "kotlin" => "kotlin",
        "c" => "c",
        "cpp" | "c++" | "cxx" => "c++",
        "cs" | "csharp" => "c#",
        "rb" | "ruby" => "ruby",
        "php" => "php",
        "sh" | "bash" | "shell" => "bash",
        "zsh" => "shell",
        "ps1" | "powershell" => "powershell",
        "html" => "html",
        "css" => "css",
        "scss" | "sass" => "sass",
        "json" => "json",
        "yaml" | "yml" => "yaml",
        "toml" => "toml",
        "xml" => "xml",
        "md" | "markdown" => "markdown",
        "sql" => "sql",
        "diff" | "patch" => "diff",
        "dockerfile" => "docker",
        "makefile" => "makefile",
        "swift" => "swift",
        "lua" => "lua",
        "scala" => "scala",
        "r" => "r",
        _ => "plain text",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;

    fn first_block_type(md: &str) -> Option<String> {
        let blocks = markdown_to_blocks(md);
        blocks
            .first()
            .and_then(|b| b.get("type"))
            .and_then(|v| v.as_str())
            .map(String::from)
    }

    #[test]
    fn 段落() {
        let blocks = markdown_to_blocks("Hello, world.");
        assert_eq!(blocks.len(), 1);
        assert_eq!(blocks[0]["type"], "paragraph");
    }

    #[test]
    fn 見出し() {
        assert_eq!(first_block_type("# H1").as_deref(), Some("heading_1"));
        assert_eq!(first_block_type("## H2").as_deref(), Some("heading_2"));
        assert_eq!(first_block_type("### H3").as_deref(), Some("heading_3"));
        // H4 以降は H3 に丸める
        assert_eq!(first_block_type("#### H4").as_deref(), Some("heading_3"));
    }

    #[test]
    fn 太字_斜体_打ち消し() {
        let blocks = markdown_to_blocks("**bold** *italic* ~~strike~~");
        let rt = &blocks[0]["paragraph"]["rich_text"];
        // 各装飾の rich_text セグメントが存在することを確認
        let texts: Vec<&Value> = rt.as_array().unwrap().iter().collect();
        assert!(texts.iter().any(|v| v["annotations"]["bold"] == true));
        assert!(texts.iter().any(|v| v["annotations"]["italic"] == true));
        assert!(
            texts
                .iter()
                .any(|v| v["annotations"]["strikethrough"] == true)
        );
    }

    #[test]
    fn インラインコード() {
        let blocks = markdown_to_blocks("This is `code`.");
        let rt = &blocks[0]["paragraph"]["rich_text"];
        let arr = rt.as_array().unwrap();
        assert!(arr.iter().any(|v| v["annotations"]["code"] == true));
    }

    #[test]
    fn 箇条書き() {
        let md = "- item 1\n- item 2";
        let blocks = markdown_to_blocks(md);
        assert_eq!(blocks.len(), 2);
        assert_eq!(blocks[0]["type"], "bulleted_list_item");
        assert_eq!(blocks[1]["type"], "bulleted_list_item");
    }

    #[test]
    fn 番号付きリスト() {
        let md = "1. one\n2. two";
        let blocks = markdown_to_blocks(md);
        assert_eq!(blocks.len(), 2);
        assert_eq!(blocks[0]["type"], "numbered_list_item");
    }

    #[test]
    fn 入れ子リスト() {
        let md = "- parent\n  - child";
        let blocks = markdown_to_blocks(md);
        assert_eq!(blocks.len(), 1);
        assert_eq!(blocks[0]["type"], "bulleted_list_item");
        let children = &blocks[0]["bulleted_list_item"]["children"];
        assert!(children.is_array());
        assert_eq!(children[0]["type"], "bulleted_list_item");
    }

    #[test]
    fn チェックボックス() {
        let md = "- [x] done\n- [ ] todo";
        let blocks = markdown_to_blocks(md);
        assert_eq!(blocks.len(), 2);
        assert_eq!(blocks[0]["type"], "to_do");
        assert_eq!(blocks[0]["to_do"]["checked"], true);
        assert_eq!(blocks[1]["to_do"]["checked"], false);
    }

    #[test]
    fn 引用() {
        let blocks = markdown_to_blocks("> quoted text");
        assert_eq!(blocks[0]["type"], "quote");
    }

    #[test]
    fn コードブロック() {
        let md = "```rust\nfn main() {}\n```";
        let blocks = markdown_to_blocks(md);
        assert_eq!(blocks[0]["type"], "code");
        assert_eq!(blocks[0]["code"]["language"], "rust");
    }

    #[test]
    fn コードブロック_未知言語は_plain_text() {
        let md = "```nonexistentlang\ncode\n```";
        let blocks = markdown_to_blocks(md);
        assert_eq!(blocks[0]["code"]["language"], "plain text");
    }

    #[test]
    fn 区切り線() {
        let blocks = markdown_to_blocks("---");
        assert_eq!(blocks[0]["type"], "divider");
    }

    #[test]
    fn リンク() {
        let blocks = markdown_to_blocks("[Anthropic](https://www.anthropic.com)");
        let rt = &blocks[0]["paragraph"]["rich_text"][0];
        assert_eq!(rt["text"]["link"]["url"], "https://www.anthropic.com");
    }

    #[test]
    fn 箇条書きの本文が_rich_text_に入る() {
        // tight list の本文（Paragraph で包まれず Text が直接来る）が
        // 正しく rich_text に格納されることを確認
        let blocks = markdown_to_blocks("- item 1\n- item 2");
        assert_eq!(blocks.len(), 2);
        let rt0 = blocks[0]["bulleted_list_item"]["rich_text"]
            .as_array()
            .unwrap();
        assert!(!rt0.is_empty());
        assert_eq!(rt0[0]["text"]["content"], "item 1");
    }

    #[test]
    fn 入れ子リストの親の本文が保持される() {
        let blocks = markdown_to_blocks("- parent\n  - child");
        assert_eq!(blocks.len(), 1);
        let parent_rt = blocks[0]["bulleted_list_item"]["rich_text"]
            .as_array()
            .unwrap();
        assert_eq!(parent_rt[0]["text"]["content"], "parent");
        let children = &blocks[0]["bulleted_list_item"]["children"];
        assert_eq!(children[0]["type"], "bulleted_list_item");
        let child_rt = children[0]["bulleted_list_item"]["rich_text"]
            .as_array()
            .unwrap();
        assert_eq!(child_rt[0]["text"]["content"], "child");
    }

    #[test]
    fn 末尾の空行で空リスト項目が生成されない() {
        let blocks = markdown_to_blocks("- item 1\n- item 2\n\n");
        let count = blocks
            .iter()
            .filter(|b| b["type"] == "bulleted_list_item")
            .count();
        assert_eq!(count, 2);
        for b in &blocks {
            if b["type"] == "bulleted_list_item" {
                let rt = b["bulleted_list_item"]["rich_text"].as_array().unwrap();
                assert!(
                    !rt.is_empty(),
                    "空の bulleted_list_item が生成された: {b:?}"
                );
            }
        }
    }
}
