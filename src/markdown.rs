//! Markdown と Notion blocks を相互変換する。
//!
//! 設計方針:
//! - サポートする要素を意図的に絞り、仕様を予測可能にする
//!   （見出し H1-H3, 段落, 太字/斜体/打ち消し/inline code,
//!    箇条書き/番号付き(入れ子含む), 引用, コードブロック,
//!    リンク, チェックボックス, 区切り線）
//! - 画像 / 表 / HTML / フットノートはサポートしない
//! - 双方向変換が完全に対称である必要はない（Notion → Markdown はベストエフォート、表示用の用途）

pub mod from_blocks;
pub mod to_blocks;

pub use from_blocks::blocks_to_markdown;
pub use to_blocks::markdown_to_blocks;
