# syntax=docker/dockerfile:1.7

FROM rust:1-bookworm AS builder

WORKDIR /app/clankcord
RUN apt-get update \
    && apt-get install -y --no-install-recommends \
        autoconf \
        automake \
        build-essential \
        cmake \
        libopus-dev \
        libtool \
        m4 \
        pkg-config \
    && rm -rf /var/lib/apt/lists/*
COPY clankcord/Cargo.toml clankcord/Cargo.lock ./
RUN --mount=type=cache,id=clankcord-cargo-registry,target=/usr/local/cargo/registry \
    --mount=type=cache,id=clankcord-cargo-git,target=/usr/local/cargo/git/db \
    target="$(rustc -vV | sed -n 's/^host: //p')" \
    && cargo fetch --locked --target "$target"
COPY clankcord/docs ./docs
COPY clankcord/src ./src
RUN --mount=type=cache,id=clankcord-cargo-registry,target=/usr/local/cargo/registry \
    --mount=type=cache,id=clankcord-cargo-git,target=/usr/local/cargo/git/db \
    --mount=type=cache,id=clankcord-target-release,target=/app/clankcord/target \
    cargo build --locked --release --bin clankcord \
    && mkdir -p /out \
    && cp /app/clankcord/target/release/clankcord /out/clankcord

FROM node:24-bookworm-slim AS runtime

RUN apt-get update \
    && apt-get install -y --no-install-recommends bash ca-certificates curl jq libopus0 postgresql-client ripgrep \
    && rm -rf /var/lib/apt/lists/* \
    && npm install -g @openai/codex@0.130.0 \
    && mkdir -p /clankcord/durable /clankcord/state /codex /workspace \
    && chmod -R 0777 /clankcord /codex /workspace

COPY --from=builder /out/clankcord /usr/local/bin/clankcord

ENV CODEX_HOME=/codex
ENV HOME=/codex
ENV CLANKCORD_CODEX_BIN=codex
ENV CLANKCORD_API_BASE_URL=http://127.0.0.1:8091

EXPOSE 8091

ENTRYPOINT ["clankcord"]
CMD ["start"]
