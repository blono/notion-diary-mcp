//! Notion API との通信を担うモジュール。
//!
//! 設計方針:
//! - 必要最小限のエンドポイントだけをラップする（監査しやすさ重視）
//! - レスポンスは serde_json::Value で受けて、必要箇所だけ型付きで取り出す。
//!   Notion の block 型は数十種あり、すべてに型を付けると重い。
//!   日記用途で使う型だけ薄く定義する。
//! - エラーは AppError::NotionApi / Http に集約する

pub mod client;
pub mod types;

pub use client::NotionClient;
