//! Streamable HTTP トランスポートでサーバを起動する。
//!
//! 用途:
//! - claude.ai / ChatGPT 等のリモート MCP クライアント
//! - Google Cloud Run 等のコンテナホスティング環境
//!
//! 設計方針:
//! - rmcp の `StreamableHttpService` を axum に nest して使う。
//!   StreamableHttpService は tower::Service として実装されているため、
//!   axum の middleware（Basic 認証）と組み合わせることができる。
//! - 認証は HTTP Basic 認証方式。資格情報は `MCP_AUTH_USER` / `MCP_AUTH_PASSWORD`
//!   環境変数から読む。未設定の場合は起動を拒否する（リモート公開時の事故防止）。
//! - claude.ai のカスタムコネクタには Authorization ヘッダを直接指定する UI がない。
//!   ただし `https://user:pass@host/mcp` 形式の URL を登録すると、claude.ai 側で
//!   自動的に `Authorization: Basic base64(user:pass)` ヘッダを付与してくれる。
//!   これを利用して、固定資格情報の Basic 認証で運用する。
//! - サーバインスタンスは StreamableHttpService の service_factory に渡す
//!   クロージャから生成する。同じ DiaryServer を全セッションで共有する形にする。
//!   （DiaryServer は Clone 可、内部状態は Arc で共有されている）

use crate::server::DiaryServer;
use axum::{
    Router,
    extract::{Request, State},
    http::{HeaderMap, StatusCode},
    middleware::{self, Next},
    response::{IntoResponse, Response},
};
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;
use reqwest::header;
use rmcp::transport::{
    StreamableHttpServerConfig,
    streamable_http_server::{StreamableHttpService, session::local::LocalSessionManager},
};
use std::sync::Arc;
use subtle::ConstantTimeEq as _;
use tracing::{info, warn};

/// HTTP transport の設定。
pub struct HttpConfig {
    /// バインドアドレス（例: "0.0.0.0:8080"）
    pub bind: String,
    /// Basic 認証のユーザー名
    pub auth_user: String,
    /// Basic 認証のパスワード（漏えい防止のため Debug は出力しない）
    pub auth_password: String,
}

impl std::fmt::Debug for HttpConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HttpConfig")
            .field("bind", &self.bind)
            .field("auth_user", &self.auth_user)
            .field("auth_password", &"***REDACTED***")
            .finish()
    }
}

/// Streamable HTTP transport でサーバを起動する。
///
/// エンドポイント:
/// - `POST /mcp`: MCP JSON-RPC リクエスト（要 Basic 認証）
/// - `GET  /mcp`: SSE ストリーミングレスポンス（要 Basic 認証）
/// - `DELETE /mcp`: セッション終了（要 Basic 認証）
/// - `GET /healthz`: ヘルスチェック（認証不要、Cloud Run 用）
pub async fn run(
    server_impl: DiaryServer,
    http_config: HttpConfig,
    allowed_hosts: Option<Vec<String>>,
) -> anyhow::Result<()> {
    info!(
        bind = %http_config.bind,
        auth_user = %http_config.auth_user,
        "MCP Streamable HTTP transport を開始します"
    );

    // rmcp の Streamable HTTP server の config を組み立てます。
    // allowed_hosts が指定されていればそれで上書きし、 未指定ならデフォルトを保ちます。
    // デフォルトは loopback only（["localhost", "127.0.0.1", "::1"]）で、
    // これはローカル開発を想定した安全側の設定です。
    let mut config = StreamableHttpServerConfig::default();
    if let Some(hosts) = allowed_hosts {
        info!("Host header の allowlist を上書きします: {hosts:?}");
        config.allowed_hosts = hosts;
    }

    // -----------------------------------------------------------------------
    // 1. rmcp の StreamableHttpService を構築
    // -----------------------------------------------------------------------
    // service_factory: 新しいセッションが来るたびに呼ばれる。
    //   ここでは事前に構築済みの DiaryServer を clone して返す。
    //   DiaryServer の内部状態は Arc で共有されているので、clone はコスト的に問題ない。
    // session_manager: セッション ID とインスタンスの対応を管理する。
    //   single-process の用途では LocalSessionManager で十分。
    let mcp_service = StreamableHttpService::new(
        move || Ok(server_impl.clone()),
        Arc::new(LocalSessionManager::default()),
        config,
    );

    // -----------------------------------------------------------------------
    // 2. 認証ミドルウェア用の State
    // -----------------------------------------------------------------------
    // axum の middleware から参照するため、Arc にラップして state 化します。
    let http_config_state: Arc<HttpConfig> = Arc::new(http_config);

    // -----------------------------------------------------------------------
    // 3. ルーティング
    // -----------------------------------------------------------------------
    // /mcp 配下に StreamableHttpService を nest する。
    // 認証ミドルウェアは /mcp 配下にのみ適用し、/healthz は認証不要。
    let public_routes = Router::new()
        // ヘルスチェック（Cloud Run のスタートアッププローブで使う）
        .route("/health", axum::routing::get(health_check));
    let private_routes =
        Router::new()
            .nest_service("/mcp", mcp_service)
            .layer(middleware::from_fn_with_state(
                http_config_state.clone(),
                basic_auth_middleware,
            ));
    let app = public_routes.merge(private_routes);

    // -----------------------------------------------------------------------
    // 4. TCP リスナーを bind して axum で起動
    // -----------------------------------------------------------------------
    let listener = tokio::net::TcpListener::bind(&http_config_state.bind)
        .await
        .map_err(|e| {
            anyhow::anyhow!(
                "バインドに失敗しました（bind={}）: {e}",
                http_config_state.bind
            )
        })?;

    info!(
        bind = %http_config_state.bind,
        "HTTP server がリクエストを待機しています"
    );

    // SIGTERM / SIGINT で graceful shutdown する（Cloud Run は SIGTERM で終了通知してくる）
    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await?;

    info!("HTTP transport を終了します");

    Ok(())
}

// =============================================================================
// 認証ミドルウェア
// =============================================================================

/// Basic 認証を検証するミドルウェア。
///
/// 仕様:
/// - `Authorization: Basic <base64(user:password)>` ヘッダを確認する
/// - 期待値と完全一致すれば次のハンドラに進む
/// - 一致しない or ヘッダが無い場合は 401 Unauthorized を返す
///   `WWW-Authenticate: Basic realm="..."` を返すことで、ブラウザ等で
///   標準の Basic 認証ダイアログが出るようにする（claude.ai の挙動とは無関係だが、
///   仕様準拠 / デバッグしやすさ のために付ける）
///
/// セキュリティ:
/// - 比較は constant-time で行う（タイミング攻撃耐性）
async fn basic_auth_middleware(
    State(expected): State<Arc<HttpConfig>>,
    req: Request,
    next: Next,
) -> Response {
    // 借用スコープを限定するため、判定だけ先に済ませてから req を next.run に渡します。
    // （req.headers() の参照が生きている間は req を move できないためです）
    let auth_ok = check_basic_auth(req.headers(), &expected);

    if auth_ok {
        next.run(req).await
    } else {
        // ログ用に method と path を所有権付きでコピーします。
        let method = req.method().clone();
        let path = req.uri().path().to_owned();
        warn!("認証に失敗しました: {method} {path}");

        // RFC 7617 に従い、401 には WWW-Authenticate ヘッダを含めます。
        // realm はクライアントへの表示用なので、識別できる名前なら何でも構いません。
        (
            StatusCode::UNAUTHORIZED,
            [(header::WWW_AUTHENTICATE, r#"Basic realm="MCP""#)],
        )
            .into_response()
    }
}

/// `Authorization` ヘッダを parse し、Basic 認証の credential が一致するかを判定します。
///
/// 返り値が true なら認証成功です。 ヘッダ未設定、形式不正、credential 不一致など、
/// 失敗する全パターンで false を返します。
fn check_basic_auth(headers: &HeaderMap, expected: &HttpConfig) -> bool {
    // 1. Authorization ヘッダから "Basic " プレフィックスを剥がす
    let encoded = match headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|s| {
            let (scheme, encoded) = s.split_once(' ')?;
            scheme.eq_ignore_ascii_case("basic").then(|| encoded.trim())
        }) {
        Some(s) => s,
        None => return false,
    };

    // 2. base64 デコード
    let decoded = match BASE64.decode(encoded) {
        Ok(b) => b,
        Err(_) => return false,
    };

    // 3. UTF-8 として解釈
    //    RFC 7617 では charset の指定がない場合 ISO-8859-1 ですが、
    //    現代のクライアントはほぼ UTF-8 を送ると思われるため、ここでは UTF-8 のみ受け入れます。
    let decoded_str = match std::str::from_utf8(&decoded) {
        Ok(s) => s,
        Err(_) => return false,
    };

    // 4. "user:password" 形式を分解
    //    password 側にコロンが含まれる可能性があるため、 最初の `:` でだけ分割します。
    let (user, password) = match decoded_str.split_once(':') {
        Some(t) => t,
        None => return false,
    };

    // 5. constant-time comparison で照合
    //    通常の == や単純なバイト列比較は、長さの違い・内容の違いの両方が
    //    処理時間に漏れ、timing 攻撃の足がかりになります。
    //    ここでは両辺を SHA-256 でハッシュして固定長（32 byte）にしてから
    //    subtle::ConstantTimeEq で比較することで、長さも内容も漏らしません。
    let user_match = hash_eq(user, &expected.auth_user);
    let pass_match = hash_eq(password, &expected.auth_password);

    // & にすることで、user_match が false でも pass_match の評価を省略しません。
    // hash_eq は常に 32 byte の ct_eq を実行するため、
    // user_match の真偽で処理時間が変わりません。
    user_match & pass_match
}

/// 2 つの文字列を constant-time で比較します。
///
/// 両辺を SHA-256 でハッシュして固定長（32 byte）に揃えてから
/// `subtle::ConstantTimeEq` で比較します。
/// これにより、長さの違いも内容の違いも処理時間に漏れません。
fn hash_eq(lhs: &str, rhs: &str) -> bool {
    use sha2::{Digest, Sha256};

    // ハッシュ化して固定長にしてから比較します。
    // 元の長さの違いはハッシュ後には見えなくなるため、
    // 長さ起因の timing leak がなくなります。
    let lhs_hash = Sha256::digest(lhs.as_bytes());
    let rhs_hash = Sha256::digest(rhs.as_bytes());

    lhs_hash.ct_eq(&rhs_hash).into()
}

// =============================================================================
// ヘルスチェック
// =============================================================================

/// 単純なヘルスチェック。Cloud Run のスタートアッププローブ用。
async fn health_check() -> &'static str {
    "ok"
}

// =============================================================================
// シャットダウンシグナル
// =============================================================================

/// SIGINT / SIGTERM を待機する。
/// Cloud Run はインスタンス終了時に SIGTERM を送ってくるので、両方対応しておく。
async fn shutdown_signal() {
    let ctrl_c = async {
        tokio::signal::ctrl_c()
            .await
            .expect("Ctrl+C ハンドラの登録に失敗しました");
    };

    #[cfg(unix)]
    let terminate = async {
        tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .expect("SIGTERM ハンドラの登録に失敗しました")
            .recv()
            .await;
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => { info!("SIGINT を受信しました。シャットダウンします"); }
        _ = terminate => { info!("SIGTERM を受信しました。シャットダウンします"); }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn constant_time_eq_同一バイト列() {
        assert!(hash_eq("hello", "hello"));
    }

    #[test]
    fn constant_time_eq_異なるバイト列() {
        assert!(!hash_eq("hello", "world"));
    }

    #[test]
    fn constant_time_eq_異なる長さ() {
        assert!(!hash_eq("hello", "hello!"));
    }

    #[test]
    fn constant_time_eq_空() {
        assert!(hash_eq("", ""));
        assert!(!hash_eq("", "a"));
    }

    #[test]
    fn basic_認証ヘッダ生成() {
        // RFC 7617 の例: "Aladdin:open sesame" → "QWxhZGRpbjpvcGVuIHNlc2FtZQ=="
        let encoded = BASE64.encode("Aladdin:open sesame");
        assert_eq!(encoded, "QWxhZGRpbjpvcGVuIHNlc2FtZQ==");
    }
}
