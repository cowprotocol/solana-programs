FROM docker.io/debian:trixie-slim
RUN apt-get update && \
    apt-get install -y --no-install-recommends ca-certificates curl jq tini && \
    rm -rf /var/lib/apt/lists/*

# The binary comes from the named "binaries" build context
# (--build-context binaries=target/release), built outside the image.
COPY --from=binaries cow /usr/local/bin/cow

ENTRYPOINT ["/usr/bin/tini", "-s", "--"]
CMD ["cow", "--help"]
