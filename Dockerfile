# 設計判断:
# - rustls を使うので OpenSSL 不要。glibc 依存だけ気にすればよい。
# - 非 root ユーザーで実行（Cloud Run のセキュリティベストプラクティス）
# =============================================================================

# -----------------------------------------------------------------------------
# Stage 1: 依存ビルド用の planner（cargo-chef 風の手動キャッシュ）
# -----------------------------------------------------------------------------
# 注意: cargo-chef は便利だが余分な依存を増やすので、ここでは
# 「依存だけ先にビルドする」シンプルな手動アプローチを採る。
FROM rust:slim-bookworm AS builder

# 作業ディレクトリ
WORKDIR /app

RUN apt-get update && \
    apt-get install -y --no-install-recommends \
        build-essential && \
    rm -rf /var/lib/apt/lists/*

# まず Cargo.toml と Cargo.lock だけコピーし、ダミーの main.rs で依存だけビルド。
# これにより、ソースコードの変更だけでは依存の再ビルドが起きない。
COPY Cargo.toml Cargo.lock ./

# ダミー main.rs を作って依存だけビルド
RUN mkdir src \
 && echo "fn main() {}" > src/main.rs \
 && cargo build --release \
 && rm -rf src target/release/notion-diary-mcp* target/release/deps/notion_diary_mcp*

# 本物のソースをコピーしてビルド
COPY src ./src
RUN touch src/main.rs && \
    cargo build --release

# -----------------------------------------------------------------------------
# Stage 2: runtime
# -----------------------------------------------------------------------------
FROM gcr.io/distroless/cc-debian13:nonroot

# バイナリだけコピー
COPY --from=builder /app/target/release/notion-diary-mcp /app/notion-diary-mcp

# Cloud Run は環境変数 PORT を自動セットしてくる（デフォルト 8080）。
# main.rs の resolve_http_bind が PORT を読んで 0.0.0.0:$PORT で listen する。
# EXPOSE は Cloud Run には影響しないがドキュメント目的で記載する。
EXPOSE 8080

# distroless には shell が無いので、ENTRYPOINT / CMD は exec 形式（JSON 配列）で書く。
# CMD で --transport=http をデフォルトに固定し、
# 環境変数だけで運用できるようにする。
ENTRYPOINT ["/app/notion-diary-mcp"]
CMD ["--transport=http"]
