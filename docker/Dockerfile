FROM rust
LABEL maintainer="PolySwarm Developers <info@polyswarm.io>"

WORKDIR /usr/src/polyswarm-relay
COPY . .
RUN cargo install

CMD ["polyswarm-relay"]
