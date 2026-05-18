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
COPY docs /app/docs
COPY clankcord/src ./src
RUN --mount=type=cache,id=clankcord-cargo-registry,target=/usr/local/cargo/registry \
    --mount=type=cache,id=clankcord-cargo-git,target=/usr/local/cargo/git/db \
    --mount=type=cache,id=clankcord-target-release,target=/app/clankcord/target \
    cargo build --locked --release --bin clankcord \
    && mkdir -p /out \
    && cp /app/clankcord/target/release/clankcord /out/clankcord

FROM node:24-bookworm-slim AS runtime

RUN apt-get update \
    && apt-get install -y --no-install-recommends bash ca-certificates clang curl jq libopus0 postgresql-client python-is-python3 python3 ripgrep zip \
    && rm -rf /var/lib/apt/lists/* \
    && npm install -g @openai/codex@0.130.0 \
    && mkdir -p /app /clankcord/durable /clankcord/state /codex /workspace \
    && chmod -R 0777 /app /clankcord /codex /workspace

COPY --from=builder /out/clankcord /usr/local/bin/clankcord
COPY config.toml config.ex.toml /app/

WORKDIR /app

EXPOSE 8091

ENTRYPOINT ["clankcord"]
CMD ["start"]
