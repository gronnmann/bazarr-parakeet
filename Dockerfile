FROM rust:1.96-trixie AS build
WORKDIR /app
RUN apt-get update && apt-get install -y --no-install-recommends libssl-dev pkg-config && rm -rf /var/lib/apt/lists/*
COPY Cargo.toml Cargo.lock ./
COPY src ./src
RUN cargo build --locked --release

FROM debian:trixie-slim AS runtime
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates curl ffmpeg libgomp1 libssl3t64 \
    && rm -rf /var/lib/apt/lists/* \
    && useradd --uid 10001 --create-home parakeet \
    && mkdir -p /data /models && chown parakeet:parakeet /data /models
COPY --from=build /app/target/release/bazarr-parakeet /usr/local/bin/bazarr-parakeet
WORKDIR /app
USER parakeet
ENV BIND=0.0.0.0:9000 MODEL_DIR=/models/ultra DATA_DIR=/data
EXPOSE 9000
HEALTHCHECK --interval=30s --timeout=3s CMD curl --fail --silent http://127.0.0.1:9000/health || exit 1
ENTRYPOINT ["bazarr-parakeet"]
