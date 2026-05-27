//! JST (Asia/Tokyo) を中心とした時刻ユーティリティ。
//!
//! 設計方針:
//! - サーバー側で「今日」「未来」「過去」の判定をすべて JST で行う。
//!   クライアント (MCP Client) の時刻判断は信用しない。
//! - 曜日表記は日本語 1 文字（"月火水木金土日"）を使用する。

use crate::error::{AppError, AppResult};
use chrono::{Datelike, NaiveDate};
use chrono_tz::Asia::Tokyo;

/// JST における現在の日付を返す。
pub fn today_jst() -> NaiveDate {
    chrono::Utc::now().with_timezone(&Tokyo).date_naive()
}

/// "YYYY-MM-DD" 形式の文字列を JST 上の日付として解釈する。
///
/// タイムゾーン情報がない日付なので、解釈そのものは TZ 非依存だが、文脈は常に JST。
pub fn parse_date(s: &str) -> AppResult<NaiveDate> {
    let invalid = || AppError::InvalidInput(format!("...{s}"));

    // 長さ・区切り位置・各フィールドが数字かを一括確認
    let b = s.as_bytes();
    if b.len() != 10
        || b[4] != b'-'
        || b[7] != b'-'
        || !b[0..4].iter().all(u8::is_ascii_digit)
        || !b[5..7].iter().all(u8::is_ascii_digit)
        || !b[8..10].iter().all(u8::is_ascii_digit)
    {
        return Err(invalid());
    }

    NaiveDate::parse_from_str(s, "%Y-%m-%d").map_err(|_| invalid())
}

/// 日本語の曜日 1 文字を返す ("月火水木金土日")。
pub fn jp_weekday(date: NaiveDate) -> &'static str {
    use chrono::Weekday::*;
    match date.weekday() {
        Mon => "月",
        Tue => "火",
        Wed => "水",
        Thu => "木",
        Fri => "金",
        Sat => "土",
        Sun => "日",
    }
}

/// Notion の日記内で使う見出しテキストを生成する。
/// 例: 2026-05-09 (土) → "2026/05/09(土)"
pub fn diary_heading_text(date: NaiveDate) -> String {
    format!(
        "{:04}/{:02}/{:02}({})",
        date.year(),
        date.month(),
        date.day(),
        jp_weekday(date)
    )
}

/// 月ページのタイトルを生成する。
/// 例: 2026-05-09 → "2026/05"
pub fn month_page_title(date: NaiveDate) -> String {
    format!("{:04}/{:02}", date.year(), date.month())
}

/// 日付に対する書き込み許可をチェックする。
///
/// - 未来日付: 拒否
/// - 過去日上限 (max_past_days > 0) を超えた日付: 拒否
/// - それ以外: OK
pub fn validate_writable_date(date: NaiveDate, max_past_days: i64) -> AppResult<()> {
    let today = today_jst();
    if date > today {
        return Err(AppError::FutureDate {
            date: date.format("%Y-%m-%d").to_string(),
        });
    }
    if max_past_days > 0 {
        let diff = (today - date).num_days();
        if diff > max_past_days {
            return Err(AppError::PastDateTooFar {
                date: date.format("%Y-%m-%d").to_string(),
                max_days: max_past_days,
            });
        }
    }
    Ok(())
}

/// 与えた日付が JST 上の「今日」かどうか。
pub fn is_today_jst(date: NaiveDate) -> bool {
    date == today_jst()
}

/// 期間内の年月をユニークに列挙する（古い順）。
/// 例: 2026-04-28 〜 2026-05-03 → [(2026,4), (2026,5)]
pub fn months_in_range(from: NaiveDate, to: NaiveDate) -> Vec<(i32, u32)> {
    let mut result = Vec::new();
    let mut cur_y = from.year();
    let mut cur_m = from.month();
    let to_y = to.year();
    let to_m = to.month();
    loop {
        result.push((cur_y, cur_m));
        if cur_y == to_y && cur_m == to_m {
            break;
        }
        // 次の月へ
        if cur_m == 12 {
            cur_m = 1;
            cur_y += 1;
        } else {
            cur_m += 1;
        }
        // 安全弁（本来到達しない）
        if result.len() > 120 {
            break;
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_date_正常系() {
        let d = parse_date("2026-05-09").unwrap();
        assert_eq!(d.year(), 2026);
        assert_eq!(d.month(), 5);
        assert_eq!(d.day(), 9);
    }

    #[test]
    fn parse_date_異常系() {
        assert!(parse_date("2026/05/09").is_err());
        assert!(parse_date("2026-5-9").is_err());
        assert!(parse_date("not a date").is_err());
    }

    #[test]
    fn diary_heading_text_フォーマット() {
        let d = NaiveDate::from_ymd_opt(2026, 5, 9).unwrap();
        // 2026-05-09 は土曜日
        assert_eq!(diary_heading_text(d), "2026/05/09(土)");
    }

    #[test]
    fn month_page_title_フォーマット() {
        let d = NaiveDate::from_ymd_opt(2026, 5, 9).unwrap();
        assert_eq!(month_page_title(d), "2026/05");
    }

    #[test]
    fn months_in_range_同月() {
        let from = NaiveDate::from_ymd_opt(2026, 5, 1).unwrap();
        let to = NaiveDate::from_ymd_opt(2026, 5, 31).unwrap();
        assert_eq!(months_in_range(from, to), vec![(2026, 5)]);
    }

    #[test]
    fn months_in_range_月跨ぎ() {
        let from = NaiveDate::from_ymd_opt(2026, 4, 28).unwrap();
        let to = NaiveDate::from_ymd_opt(2026, 5, 3).unwrap();
        assert_eq!(months_in_range(from, to), vec![(2026, 4), (2026, 5)]);
    }

    #[test]
    fn months_in_range_年跨ぎ() {
        let from = NaiveDate::from_ymd_opt(2025, 12, 30).unwrap();
        let to = NaiveDate::from_ymd_opt(2026, 1, 2).unwrap();
        assert_eq!(months_in_range(from, to), vec![(2025, 12), (2026, 1)]);
    }
}
