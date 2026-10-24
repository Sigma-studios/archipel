# archipel's image: the web UI (WebAssembly) and the server that embeds it, on a
# slim Debian with what game servers need (Fistful of Frags wants 32-bit
# libraries, Source servers libcurl and libtinfo).

FROM rust:1-bookworm AS build
ARG TRUNK_VERSION=0.21.14
RUN rustup target add wasm32-unknown-unknown \
 && arch=$(uname -m) \
 && curl -fsSL "https://github.com/trunk-rs/trunk/releases/download/v${TRUNK_VERSION}/trunk-${arch}-unknown-linux-gnu.tar.gz" \
    | tar -xz -C /usr/local/bin trunk
WORKDIR /src
COPY . .
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/src/target \
    cd crates/archipel-ui && trunk build --release \
 && cd /src && cargo build --release -p archipel \
 && cp target/release/archipel /usr/local/bin/archipel

FROM debian:bookworm-slim
RUN set -eux; extra=""; \
    if [ "$(dpkg --print-architecture)" = amd64 ]; then \
      dpkg --add-architecture i386; \
      extra="lib32gcc-s1 lib32stdc++6 libc6:i386 libcurl3-gnutls libcurl3-gnutls:i386 libtinfo5 libtinfo5:i386"; \
    fi; \
    apt-get update; \
    apt-get install -y --no-install-recommends ca-certificates tar gzip $extra; \
    rm -rf /var/lib/apt/lists/*
COPY --from=build /usr/local/bin/archipel /usr/local/bin/archipel
ENV ARCHIPEL_DATA=/data
EXPOSE 7810
CMD ["archipel"]
