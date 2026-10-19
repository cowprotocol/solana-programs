FROM docker.io/debian:trixie-slim

# Keep SOLANA_VERSION in sync with .github/actions/setup-solana.
ARG SOLANA_VERSION=v4.0.3
ARG TARGETARCH

# We install extra tools to allow building and giving more features to the CLI user (like `bc`). 
RUN apt-get update && \
    apt-get install -y --no-install-recommends ca-certificates curl jq bc bzip2 && \
    rm -rf /var/lib/apt/lists/*

# This isn't strictly needed but may be useful when using the CLI.
# Only available on x86_64.
RUN if [ "$TARGETARCH" = "amd64" ]; then \
        curl -sSfL -o /tmp/solana.tar.bz2 "https://release.anza.xyz/${SOLANA_VERSION}/solana-release-x86_64-unknown-linux-gnu.tar.bz2" && \
        tar -xjf /tmp/solana.tar.bz2 -C /usr/local --strip-components=1 solana-release/bin/solana && \
        rm /tmp/solana.tar.bz2; \
    fi

# The binary comes from the named "binaries" build context
# (--build-context binaries=target/release), built outside the image.
COPY --from=binaries cow /usr/local/bin/cow

CMD ["cow", "--help"]
