FROM rust:1.39.0-buster
LABEL maintainer="PolySwarm Developers <info@polyswarm.io>"

ENV RUSTC_WRAPPER=sccache
ENV SCCACHE_VERSION 0.2.7
ENV DOCKERIZE_VERSION v0.6.1

RUN wget https://github.com/mozilla/sccache/releases/download/$SCCACHE_VERSION/sccache-$SCCACHE_VERSION-x86_64-unknown-linux-musl.tar.gz \
    && tar -xzvf sccache-$SCCACHE_VERSION-x86_64-unknown-linux-musl.tar.gz \
    && mv sccache-$SCCACHE_VERSION-x86_64-unknown-linux-musl/sccache /usr/local/bin \
    && rm -rf sccache-$SCCACHE_VERSION-x86_64-unknown-linux-musl* \
    && USER=root cargo new --bin polyswarm-relay \
    # Keep dockerize for now, this should probably go away in future versions
    && wget https://github.com/jwilder/dockerize/releases/download/$DOCKERIZE_VERSION/dockerize-alpine-linux-amd64-$DOCKERIZE_VERSION.tar.gz \
    && tar -C /usr/local/bin -xzvf dockerize-alpine-linux-amd64-$DOCKERIZE_VERSION.tar.gz \
    && rm dockerize-alpine-linux-amd64-$DOCKERIZE_VERSION.tar.gz \
    && apt-get update \
    && apt-get install cmake -y

WORKDIR /polyswarm-relay

## Copy over your manifests
COPY Cargo.lock Cargo.toml rustfmt.toml ./

# This build step will cache your dependencies
RUN cargo build --release \
    && sccache --show-stats

RUN rm -r src/
COPY ./src ./src

RUN find ./src -type f -exec touch {} \; \
    && cargo build --release \
    && cp /polyswarm-relay/target/release/polyswarm-relay /usr/local/bin

# These are being copied here due to default configurations in "orchestration"
# TODO: "orchestration" should not provide explicit or absolte paths
COPY ./docker/config.toml ./config.toml
COPY ./docker/keyfiles /keyfiles

COPY lint.sh test.sh coverage.sh audit.sh ./

CMD ["polyswarm-relay", "--config", "config.toml"]
