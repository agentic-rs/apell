FROM docker.io/oven/bun:1.3.13 AS bun

FROM docker.io/library/rust:1.99.0-trixie
RUN apt-get update && apt-get install -y --no-install-recommends cmake \
  && rm -rf /var/lib/apt/lists/*
COPY --from=bun /usr/local/bin/bun /usr/local/bin/bun
COPY entrypoint.sh /usr/local/bin/apell-local
WORKDIR /workspace
ENTRYPOINT ["bash", "/usr/local/bin/apell-local"]
