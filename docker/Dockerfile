FROM rust
LABEL maintainer="PolySwarm Developers <info@polyswarm.io>"

# # create a new empty shell project
RUN USER=root cargo new --bin relay
WORKDIR /relay

# copy over your manifests
COPY ./Cargo.lock ./Cargo.lock
COPY ./Cargo.toml ./Cargo.toml

# this build step will cache your dependencies
RUN cargo build --release
RUN rm src/*.rs

# copy your source tree
COPY ./src ./src

# build for release
RUN cargo build --release

RUN cargo install

CMD ["polyswarm-relay", "docker/config.toml"]
