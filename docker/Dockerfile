FROM rust
LABEL maintainer="PolySwarm Developers <info@polyswarm.io>"

ENV SCCACHE_VERSION 0.2.7
RUN wget https://github.com/mozilla/sccache/releases/download/$SCCACHE_VERSION/sccache-$SCCACHE_VERSION-x86_64-unknown-linux-musl.tar.gz \
    && tar -xzvf sccache-$SCCACHE_VERSION-x86_64-unknown-linux-musl.tar.gz \
    && mv sccache-$SCCACHE_VERSION-x86_64-unknown-linux-musl/sccache /usr/local/bin \
    && rm -rf sccache-$SCCACHE_VERSION-x86_64-unknown-linux-musl*

ENV RUSTC_WRAPPER=sccache

# These potentially contain secrets from CI but are included in intermediate
# image so is fine
ARG SCCACHE_BUCKET
ENV SCCACHE_BUCET=$SCCACHE_BUCKET
ARG AWS_ACCESS_KEY_ID
ENV AWS_ACCESS_KEY_ID=$AWS_ACCESS_KEY_ID
ARG AWS_SECRET_ACCESS_KEY
ENV AWS_SECRET_ACCESS_KEY=$AWS_SECRET_ACCESS_KEY

# Create a new empty shell project
RUN USER=root cargo new --bin polyswarm-relay
WORKDIR /polyswarm-relay

# Copy over your manifests
COPY ./Cargo.lock ./Cargo.lock
COPY ./Cargo.toml ./Cargo.toml

# This build step will cache your dependencies
RUN cargo build --release \
    && sccache --show-stats

RUN rm -r src/
COPY ./src ./src

RUN find ./src -type f -exec touch {} \; && \
    cargo build --release

# Final image
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
