FROM rust
LABEL maintainer="PolySwarm Developers <info@polyswarm.io>"

ENV SCCACHE_VERSION 0.2.7
RUN wget https://github.com/mozilla/sccache/releases/download/$SCCACHE_VERSION/sccache-$SCCACHE_VERSION-x86_64-unknown-linux-musl.tar.gz \
    && tar -xzvf sccache-$SCCACHE_VERSION-x86_64-unknown-linux-musl.tar.gz \
    && mv sccache-$SCCACHE_VERSION-x86_64-unknown-linux-musl/sccache /usr/local/bin \
    && rm -rf sccache-$SCCACHE_VERSION-x86_64-unknown-linux-musl*

ENV RUSTC_WRAPPER=sccache

# # create a new empty shell project
RUN USER=root cargo new --bin polyswarm-relay
WORKDIR /polyswarm-relay

# copy over your manifests
COPY ./Cargo.lock ./Cargo.lock
COPY ./Cargo.toml ./Cargo.toml

# this build step will cache your dependencies
RUN cargo build --release \
    && sccache --show-stats

RUN rm -r src/
COPY ./src ./src

RUN find ./src -type f -exec touch {} \; && \
    cargo build --release

FROM ubuntu:bionic

RUN apt-get update && apt-get install -y \
        wget \
    && rm -rf /var/lib/apt/lists/*

ENV DOCKERIZE_VERSION v0.6.1
RUN wget https://github.com/jwilder/dockerize/releases/download/$DOCKERIZE_VERSION/dockerize-alpine-linux-amd64-$DOCKERIZE_VERSION.tar.gz \
    && tar -C /usr/local/bin -xzvf dockerize-alpine-linux-amd64-$DOCKERIZE_VERSION.tar.gz \
    && rm dockerize-alpine-linux-amd64-$DOCKERIZE_VERSION.tar.gz

COPY --from=0 /polyswarm-relay/target/release/polyswarm-relay /usr/local/bin
COPY ./docker/config.toml ./config.toml
COPY ./docker/keyfiles ./keyfiles

CMD ["polyswarm-relay", "--config", "config.toml"]
