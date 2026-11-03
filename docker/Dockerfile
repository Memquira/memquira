# Multi-stage image for the Memquira API server (`memcore-api`; no Qdrant client by default).
# Optional features via CARGO_FEATURES build arg (comma-separated):
#   postgres  — required for MEMCORE_FACT_BACKEND=postgres (production compose)
#   qdrant    — required for MEMCORE_VECTOR_BACKEND=qdrant (production compose)
# Examples:
#   docker build -f docker/Dockerfile --build-arg CARGO_FEATURES=postgres -t memcore-api:postgres .
#   docker build -f docker/Dockerfile --build-arg CARGO_FEATURES="postgres,qdrant" -t memcore-api:prod .
#   docker build -f docker/Dockerfile --build-arg APP_VERSION=0.1.0 --build-arg GIT_SHA=abc123 -t memcore:0.1.0 .
#
# Security notes: runtime runs as non-root `memcore`; do not bake secrets into the image.
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

ENV MEMCORE_BUILD_GIT_SHA=${GIT_SHA}
ENV MEMCORE_BUILD_TIMESTAMP=${BUILD_TIMESTAMP}
ENV MEMCORE_BUILD_PROFILE=release

RUN if [ -n "$CARGO_FEATURES" ]; then \
      cargo build --release -p memcore-api --features "$CARGO_FEATURES"; \
    else \
      cargo build --release -p memcore-api; \
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
    && groupadd --system --gid 10001 memcore \
    && useradd --system --uid 10001 --gid memcore --home-dir /app --shell /usr/sbin/nologin memcore

WORKDIR /app
RUN mkdir -p /app/data \
    && chown -R memcore:memcore /app

COPY --from=builder /app/target/release/memcore-api /usr/local/bin/memcore-api
# Operator-only maintenance (`docker compose exec memcore memcore-maintenance ...`).
COPY --from=builder /app/target/release/memcore-maintenance /usr/local/bin/memcore-maintenance
# Licence and third-party notices travel with the image, in the same layout as the release archives
#: the project LICENSE, licenses/NOTICE, the Rust crates' licences, and the
# Swagger UI Apache-2.0 LICENSE / NOTICE for the files served at /docs.
COPY LICENSE licenses/NOTICE licenses/THIRD_PARTY_LICENSES.txt /usr/share/doc/memcore/
COPY crates/memcore-api/assets/swagger-ui/LICENSE crates/memcore-api/assets/swagger-ui/NOTICE /usr/share/doc/memcore/swagger-ui/
COPY crates/memcore-api/assets/swagger-ui/swagger-ui-bundle.js.LICENSE.txt \
     crates/memcore-api/assets/swagger-ui/swagger-ui-es-bundle-core.js.LICENSE.txt \
     crates/memcore-api/assets/swagger-ui/swagger-ui-es-bundle.js.LICENSE.txt \
     crates/memcore-api/assets/swagger-ui/swagger-ui-standalone-preset.js.LICENSE.txt /usr/share/doc/memcore/swagger-ui/

USER memcore:memcore

EXPOSE 8080

HEALTHCHECK --interval=10s --timeout=5s --retries=5 --start-period=15s \
  CMD curl -fsS http://localhost:8080/health/ready || exit 1

CMD ["memcore-api"]
