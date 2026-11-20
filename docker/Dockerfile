# Multi-stage image for the Memquira API server (`memquira-api`; no Qdrant client by default).
# Optional features via CARGO_FEATURES build arg (comma-separated):
#   postgres  — required for MEMQUIRA_FACT_BACKEND=postgres (production compose)
#   qdrant    — required for MEMQUIRA_VECTOR_BACKEND=qdrant (production compose)
# Examples:
#   docker build -f docker/Dockerfile --build-arg CARGO_FEATURES=postgres -t memquira-api:postgres .
#   docker build -f docker/Dockerfile --build-arg CARGO_FEATURES="postgres,qdrant" -t memquira-api:prod .
#   docker build -f docker/Dockerfile --build-arg APP_VERSION=0.3.0 --build-arg GIT_SHA=abc123 -t memquira:0.3.0 .
#
# Security notes: runtime runs as non-root `memquira`; do not bake secrets into the image.
# See docs/security.md.

ARG CARGO_FEATURES=
ARG APP_VERSION=unknown
ARG GIT_SHA=unknown
ARG BUILD_TIMESTAMP=unknown

FROM rust:1.89-bookworm@sha256:948f9b08a66e7fe01b03a98ef1c7568292e07ec2e4fe90d88c07bb14563c84ff AS builder
WORKDIR /app

# Copy workspace manifests first for better layer caching.
COPY Cargo.toml Cargo.lock ./
COPY crates ./crates

ARG CARGO_FEATURES
ARG APP_VERSION
ARG GIT_SHA
ARG BUILD_TIMESTAMP

ENV MEMQUIRA_BUILD_GIT_SHA=${GIT_SHA}
ENV MEMQUIRA_BUILD_TIMESTAMP=${BUILD_TIMESTAMP}
ENV MEMQUIRA_BUILD_PROFILE=release

RUN if [ -n "$CARGO_FEATURES" ]; then \
      cargo build --release -p memquira-api --features "$CARGO_FEATURES"; \
    else \
      cargo build --release -p memquira-api; \
    fi

FROM debian:bookworm-slim@sha256:99c0b25c46d0214d5a2091092e7908907dcaecd6a962cde0ac700acee222b6f3 AS runtime

ARG APP_VERSION=unknown
ARG GIT_SHA=unknown
ARG BUILD_TIMESTAMP=unknown

LABEL org.opencontainers.image.title="memquira" \
      org.opencontainers.image.description="Memquira: persistent memory engine for AI agents" \
      org.opencontainers.image.source="https://github.com/memquira/memquira" \
      org.opencontainers.image.version="${APP_VERSION}" \
      org.opencontainers.image.revision="${GIT_SHA}" \
      org.opencontainers.image.created="${BUILD_TIMESTAMP}"

RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates curl \
    && rm -rf /var/lib/apt/lists/* \
    && groupadd --system --gid 10001 memquira \
    && useradd --system --uid 10001 --gid memquira --home-dir /app --shell /usr/sbin/nologin memquira

WORKDIR /app
RUN mkdir -p /app/data \
    && chown -R memquira:memquira /app

COPY --from=builder /app/target/release/memquira-api /usr/local/bin/memquira-api
# Operator-only maintenance (`docker compose exec memquira memquira-maintenance ...`).
COPY --from=builder /app/target/release/memquira-maintenance /usr/local/bin/memquira-maintenance
# Pre-0.3 binary names, kept so existing operator commands and scripts keep working.
RUN ln -s /usr/local/bin/memquira-api /usr/local/bin/memcore-api \
    && ln -s /usr/local/bin/memquira-maintenance /usr/local/bin/memcore-maintenance
# Licence and third-party notices travel with the image, in the same layout as the release archives
#: the project LICENSE, licenses/NOTICE, the Rust crates' licences, and the
# Swagger UI Apache-2.0 LICENSE / NOTICE for the files served at /docs.
COPY LICENSE licenses/NOTICE licenses/THIRD_PARTY_LICENSES.txt /usr/share/doc/memquira/
COPY crates/memquira-api/assets/swagger-ui/LICENSE crates/memquira-api/assets/swagger-ui/NOTICE /usr/share/doc/memquira/swagger-ui/
COPY crates/memquira-api/assets/swagger-ui/swagger-ui-bundle.js.LICENSE.txt \
     crates/memquira-api/assets/swagger-ui/swagger-ui-es-bundle-core.js.LICENSE.txt \
     crates/memquira-api/assets/swagger-ui/swagger-ui-es-bundle.js.LICENSE.txt \
     crates/memquira-api/assets/swagger-ui/swagger-ui-standalone-preset.js.LICENSE.txt /usr/share/doc/memquira/swagger-ui/

USER memquira:memquira

EXPOSE 8080

HEALTHCHECK --interval=10s --timeout=5s --retries=5 --start-period=15s \
  CMD curl -fsS http://localhost:8080/health/ready || exit 1

CMD ["memquira-api"]
