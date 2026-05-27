//! 日記操作のビジネスロジック層。
//!
//! このモジュールは Notion API クライアントとアプリのドメイン
//! （日記/月ページ/日見出し）の橋渡しを担う。
//! MCP の tool 層は、このモジュールの関数を呼ぶだけのシンプルな実装になる。

pub mod heading;
pub mod month_page;
pub mod read;
pub mod save;

pub use month_page::MonthPageResolver;
pub use read::{RecentDiary, get_recent_diary};
pub use save::{SaveDiaryOutcome, SaveResult, save_diary};
