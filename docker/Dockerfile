FROM ubuntu:xenial
LABEL maintainer="PolySwarm Developers <info@polyswarm.io>"

# Install rust
RUN set -x && \
    apt-get update && apt-get install -y \
        build-essential \
        curl \
        libssl-dev \
        pkg-config && \
    curl https://sh.rustup.rs -sSf | sh -s -- --default-toolchain stable -y && \
    rm -rf /var/lib/apt/lists/*
ENV PATH=/root/.cargo/bin:$PATH

# Install solc
RUN set -x && \
    apt-get update && apt-get install -y software-properties-common && \
    add-apt-repository ppa:ethereum/ethereum && \
    apt-get update && apt-get install -y \
        solc && \
    rm -rf /var/lib/apt/lists/*

# Install truffle
RUN set -x && \
    curl -sSf https://deb.nodesource.com/setup_9.x | bash - && \
    apt-get install -y \
        nodejs && \
    rm -rf /var/lib/apt/lists/* && \
    npm i -g truffle

WORKDIR /usr/src/polyswarm-relay
COPY . .
RUN cargo install

CMD ["polyswarm-relay", "docker/config.toml"]
