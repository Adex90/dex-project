# Synthetic Capital backend image (Rust: axum + sqlx).
#
# Build from the REPOSITORY ROOT: the SQL migrations in ../migrations are
# embedded in the binary at compile time.
#
#   docker build -f backend/Dockerfile -t synthetic-capital-backend .
#
# Base images are pinned by digest in the FROM lines, where Dependabot keeps
# them current (it cannot see an image named through an ARG). A digest is a
# content address: behind a pull-through mirror (dockerd "registry-mirrors")
# the bytes are the same. Builder and runtime are both Debian 13 (trixie).
#
# Behind a TLS-intercepting proxy, pass its CA as an optional BuildKit secret.
# It is trusted only while crates are downloaded and never lands in a layer:
#
#   --secret id=extra_ca,src=/path/to/corporate-ca.pem
#
# Runtime contract (README "Production database users", .env.example):
#   (no args) | servir    HTTP server on BIND_ADDR (default 0.0.0.0:8080)
#   migrar                one-shot job: migrations + bootstrap as the schema
#                         owner, then the app login if DB_APP_PASSWORD is set
#   salud [vivo|listo]    probe of the local server; exit 0 only on HTTP 200
# Every configuration variable NAME also accepts NAME_FILE (secrets files).
# The process writes nothing to disk: run it with a read-only root filesystem.

# --------------------------------------------------------------------------
FROM docker.io/library/rust:1.97.1-slim-trixie@sha256:8e8cf8f7fd54a2d23d5a743b3a03f56e26b6c774276c33fa0595111704ebb15c AS builder

# Release profile for the image: fat LTO, one codegen unit, debuginfo stripped
# (symbols stay, so panic messages and backtraces remain readable). panic stays
# "unwind": a panicking request handler must not take the whole server down.
ENV CARGO_PROFILE_RELEASE_LTO=fat \
    CARGO_PROFILE_RELEASE_CODEGEN_UNITS=1 \
    CARGO_PROFILE_RELEASE_OPT_LEVEL=3 \
    CARGO_PROFILE_RELEASE_DEBUG=false \
    CARGO_PROFILE_RELEASE_STRIP=debuginfo \
    CARGO_PROFILE_RELEASE_PANIC=unwind \
    CARGO_INCREMENTAL=0 \
    CARGO_TERM_COLOR=never

WORKDIR /src/backend

# 1. Dependencies only, cached until Cargo.toml or Cargo.lock change: a stub
#    crate with the real manifest downloads and compiles every dependency.
COPY backend/Cargo.toml backend/Cargo.lock backend/build.rs ./
RUN mkdir src && echo 'fn main() {}' > src/main.rs && touch src/lib.rs

RUN --mount=type=secret,id=extra_ca,required=false \
    set -eu; \
    if [ -s /run/secrets/extra_ca ]; then \
        cat /etc/ssl/certs/ca-certificates.crt /run/secrets/extra_ca > /tmp/ca-bundle.crt; \
        export CARGO_HTTP_CAINFO=/tmp/ca-bundle.crt; \
    fi; \
    cargo fetch --locked --target "$(rustc --print host-tuple)"; \
    rm -f /tmp/ca-bundle.crt

RUN cargo build --release --frozen --bin synthetic-capital

# 2. The application. COPY keeps the build context's mtimes, which can be
#    older than the stub build: touch the crate roots so cargo rebuilds them.
COPY migrations /src/migrations
COPY backend/src ./src
RUN touch src/main.rs src/lib.rs \
 && cargo build --release --frozen --bin synthetic-capital \
 && install -D -m 0755 target/release/synthetic-capital /out/synthetic-capital

# --------------------------------------------------------------------------
FROM gcr.io/distroless/cc-debian13:nonroot@sha256:3ee00291cf386ec52a552e6a858d555debd74776eb4bfa53fa3df2cd6677515e

ARG VERSION=0.1.0
ARG REVISION=unknown
ARG CREATED=unknown
ARG SOURCE=https://github.com/Adex90/dex-project
LABEL org.opencontainers.image.title="synthetic-capital-backend" \
      org.opencontainers.image.description="Synthetic Capital backend: 100% reserve IFPE and maturity-matched SOFOM (axum + sqlx)" \
      org.opencontainers.image.version="${VERSION}" \
      org.opencontainers.image.revision="${REVISION}" \
      org.opencontainers.image.created="${CREATED}" \
      org.opencontainers.image.source="${SOURCE}" \
      org.opencontainers.image.base.name="gcr.io/distroless/cc-debian13:nonroot"

COPY --from=builder /out/synthetic-capital /usr/local/bin/synthetic-capital

# distroless "nonroot": uid/gid 65532, no shell, no package manager.
USER 65532:65532
EXPOSE 8080
# Liveness only, no database: a PostgreSQL outage must not restart the
# container. Load balancers and readiness checks use GET /salud.
HEALTHCHECK --interval=15s --timeout=5s --start-period=30s --start-interval=2s --retries=3 \
    CMD ["/usr/local/bin/synthetic-capital", "salud", "vivo"]
ENTRYPOINT ["/usr/local/bin/synthetic-capital"]
