FROM rust
LABEL maintainer="PolySwarm Developers <info@polyswarm.io>"

# # create a new empty shell project
RUN USER=root cargo new --bin polyswarm-relay
WORKDIR /polyswarm-relay

# copy over your manifests
COPY ./Cargo.lock ./Cargo.lock
COPY ./Cargo.toml ./Cargo.toml

# this build step will cache your dependencies
RUN cargo build
RUN rm src/*.rs

# copy your source tree
COPY ./src ./src

# build for release
RUN cargo build --release

RUN cargo install

COPY ./docker ./docker

CMD ["polyswarm-relay", "--config", "docker/config.toml"]
