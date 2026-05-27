//! notion-diary-mcp のエントリポイント。
//!
//! 起動の流れ:
//! 1. CLI 引数をパース（`--transport=stdio` / `--transport=http`、`--bind`、`--port`）
//! 2. tracing をセットアップ
//!    - stdio 時: 必ず stderr へ出力（stdout は MCP の JSON-RPC で占有されるため）
//!    - http  時: stderr へ出力（Cloud Run も stderr を Cloud Logging に流す）
//! 3. `.env` を読み込み Config を構築
//! 4. NotionClient と DiaryServer を構築
//! 5. transport に応じてサーバを起動
//! 6. クライアント切断 / シグナル受信まで待機

mod config;
mod diary;
mod error;
mod markdown;
mod notion;
mod server;
mod time_util;
mod transport;

use crate::config::Config;
use crate::notion::NotionClient;
use crate::server::DiaryServer;
use clap::{Parser, ValueEnum};
use tracing::info;
use tracing_subscriber::{EnvFilter, fmt};

// =============================================================================
// CLI 引数定義
// =============================================================================

/// notion-diary-mcp の CLI 引数。
#[derive(Debug, Parser)]
#[command(version, about, long_about = None)]
struct Cli {
    /// トランスポート方式
    #[arg(long, value_enum, default_value_t = TransportArg::Stdio)]
    transport: TransportArg,

    /// HTTP transport の bind アドレス(host:port)。
    /// 未指定時は環境変数 MCP_HTTP_BIND を参照、それも無ければ 0.0.0.0:$PORT（Cloud Run 互換）
    /// または 0.0.0.0:8765 を使用する。
    #[arg(long)]
    bind: Option<String>,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum TransportArg {
    /// 標準入出力でやり取りする（Claude Desktop 等のローカル MCP クライアント向け）
    Stdio,
    /// Streamable HTTP でやり取りする（claude.ai / ChatGPT / Cloud Run 等のリモート向け）
    Http,
}

/// HTTP の bind アドレスを解決する。
///
/// 優先順位:
/// 1. CLI 引数 `--bind`
/// 2. 環境変数 `MCP_HTTP_BIND`
/// 3. 環境変数 `PORT`（Cloud Run が自動設定）→ `0.0.0.0:$PORT`
/// 4. デフォルト `0.0.0.0:8765`
fn resolve_http_bind(cli_bind: Option<&str>) -> String {
    if let Some(b) = cli_bind {
        return b.to_string();
    }
    if let Ok(b) = std::env::var("MCP_HTTP_BIND") {
        if !b.trim().is_empty() {
            return b;
        }
    }
    if let Ok(port) = std::env::var("PORT")
        .or_else(|_| std::env::var("CONTAINER_APP_PORT"))
        .or_else(|_| std::env::var("AWS_LWA_PORT"))
    {
        if !port.trim().is_empty() {
            return format!("0.0.0.0:{port}");
        }
    }
    "0.0.0.0:8765".to_string()
}

// =============================================================================
// main
// =============================================================================

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();

    // ----------------------------------------------------------------------
    // 1. ロガー初期化
    // ----------------------------------------------------------------------
    // stdio / http いずれの場合も stderr に出力する。
    //  - stdio: stdout は MCP プロトコルが占有するため使えない
    //  - http : Cloud Run は stderr を Cloud Logging に流すため、stderr が標準
    let env_filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new("info,notion_diary_mcp=info"));

    // http transport なら ANSI カラーコードを有効、stdio transport なら無効。
    // （Claude Desktop のログビューワは ANSI 非対応）
    let with_ansi = matches!(cli.transport, TransportArg::Http);

    fmt()
        .with_env_filter(env_filter)
        .with_writer(std::io::stderr)
        .with_ansi(with_ansi)
        .with_target(true)
        .init();

    std::panic::set_hook(Box::new(|info| {
        eprintln!("[notion-diary-mcp] PANIC: {info}");
    }));

    info!(
        version = env!("CARGO_PKG_VERSION"),
        transport = ?cli.transport,
        "notion-diary-mcp 起動"
    );

    // ----------------------------------------------------------------------
    // 2. Config 構築
    // ----------------------------------------------------------------------
    let config = match Config::from_env() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("[notion-diary-mcp] 設定エラー: {e}");
            eprintln!(
                "必要な環境変数: NOTION_TOKEN, NOTION_DIARY_DATABASE_ID\n\
                 .env.example を参考に設定してください。"
            );
            std::process::exit(2);
        }
    };
    info!(
        database_id = %config.database_id,
        max_past_days = config.max_past_days,
        "設定をロードしました"
    );

    // ----------------------------------------------------------------------
    // 3. NotionClient / DiaryServer 構築
    // ----------------------------------------------------------------------
    let client = NotionClient::new(config.clone())?;
    let server_impl = DiaryServer::new(config, client);

    // ----------------------------------------------------------------------
    // 4. transport に応じて起動
    // ----------------------------------------------------------------------
    match cli.transport {
        TransportArg::Stdio => {
            transport::stdio::run(server_impl).await?;
        }
        TransportArg::Http => {
            // HTTP transport の設定を組み立てる
            let bind = resolve_http_bind(cli.bind.as_deref());

            // Basic 認証の資格情報を環境変数から読む。
            // 両方とも必須にする（事故防止のため、片方欠けてもエラーにする）。
            let auth_user = std::env::var("MCP_AUTH_USER").map_err(|_| {
                eprintln!(
                    "[notion-diary-mcp] HTTP transport には MCP_AUTH_USER 環境変数が必須です。\n\
                     リモート公開時の事故防止のため、認証なしでの起動はできません。"
                );
                std::process::exit(2);
            })?;
            let auth_password = std::env::var("MCP_AUTH_PASSWORD").map_err(|_| {
                eprintln!(
                    "[notion-diary-mcp] HTTP transport には MCP_AUTH_PASSWORD 環境変数が必須です。"
                );
                std::process::exit(2);
            })?;

            // 空文字での起動も拒否する（"" でも env::var は Ok を返すため別途チェック）
            if auth_user.trim().is_empty() {
                eprintln!("[notion-diary-mcp] MCP_AUTH_USER が空です");
                std::process::exit(2);
            }
            if auth_password.trim().is_empty() {
                eprintln!("[notion-diary-mcp] MCP_AUTH_PASSWORD が空です");
                std::process::exit(2);
            }

            // Basic 認証の RFC では user:password の user 部分に ':' を含めることが
            // できないため、ユーザー名に ':' が含まれていたら起動を拒否する。
            if auth_user.contains(':') {
                eprintln!(
                    "[notion-diary-mcp] MCP_AUTH_USER に ':' を含めることはできません \
                     (Basic 認証の仕様上、ユーザー名とパスワードは ':' で区切られるため)"
                );
                std::process::exit(2);
            }

            // DNS rebinding 攻撃対策で、rmcp は Host header 検証を行います
            // （CVE-2026-42559 の対策、 v1.4.0 以降デフォルト有効）。
            // デフォルト allowlist は loopback only（localhost / 127.0.0.1 / ::1）のため、
            // Cloud Run など外部にデプロイする場合は、サービスのホスト名を
            // `MCP_ALLOWED_HOSTS` 環境変数（カンマ区切り）に設定してください。
            //
            // 例: MCP_ALLOWED_HOSTS=xxx.run.app,custom.example.com
            //
            // 未設定なら rmcp のデフォルト (loopback only) を維持します。
            // 設定すると指定値で完全に上書きするため、 ローカル開発も併用するなら
            // `localhost,127.0.0.1` も明示的に含めてください。
            let allowed_hosts = std::env::var("MCP_ALLOWED_HOSTS")
                .ok()
                .filter(|v| !v.is_empty())
                .map(|v| {
                    v.split(',')
                        .map(|s| s.trim().to_owned())
                        .filter(|s| !s.is_empty())
                        .collect::<Vec<_>>()
                });

            let http_config = transport::http::HttpConfig {
                bind,
                auth_user,
                auth_password,
            };
            transport::http::run(server_impl, http_config, allowed_hosts).await?;
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_http_bind_cli優先() {
        // CLI 引数が最優先
        unsafe {
            std::env::set_var("MCP_HTTP_BIND", "1.2.3.4:9999");
            std::env::set_var("PORT", "8080");
        }
        assert_eq!(resolve_http_bind(Some("127.0.0.1:1234")), "127.0.0.1:1234");
        unsafe {
            std::env::remove_var("MCP_HTTP_BIND");
            std::env::remove_var("PORT");
        }
    }

    #[test]
    fn resolve_http_bind_デフォルト() {
        unsafe {
            std::env::remove_var("MCP_HTTP_BIND");
            std::env::remove_var("PORT");
        }
        assert_eq!(resolve_http_bind(None), "0.0.0.0:8765");
    }
}
