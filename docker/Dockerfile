FROM rust
LABEL maintainer="PolySwarm Developers <info@polyswarm.io>"

# Install solc, REMOVE --allow-unsigned ONCE APT-KEY STOPS COMPLAINING
RUN apt-get update && apt-get install -y software-properties-common && \
    add-apt-repository ppa:ethereum/ethereum && \
    apt-get update && apt-get install -y --allow-unauthenticated \
        solc && \
    rm -rf /var/lib/apt/lists/*

# Install truffle
RUN curl -sL https://deb.nodesource.com/setup_9.x | bash - && \
    apt-get install -y \
        nodejs && \
    rm -rf /var/lib/apt/lists/* && \
    npm i -g truffle

WORKDIR /usr/src/polyswarm-relay
COPY . .
RUN cargo install

CMD ["polyswarm-relay"]
