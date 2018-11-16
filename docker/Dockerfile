FROM rust
LABEL maintainer="PolySwarm Developers <info@polyswarm.io>"

# # create a new empty shell project
RUN USER=root cargo new --bin polyswarm-relay
WORKDIR /polyswarm-relay

# copy over your manifests
COPY ./Cargo.lock ./Cargo.lock
COPY ./Cargo.toml ./Cargo.toml

# this build step will cache your dependencies
RUN cargo build --release

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

CMD ["polyswarm-relay", "--config", "config.toml"]
