FROM --platform=$BUILDPLATFORM rust:1.100-slim-bookworm AS build
ARG TARGETARCH
WORKDIR /build

RUN set -eu; \
    case "$TARGETARCH" in \
      amd64) target=x86_64-unknown-linux-gnu; cross="gcc-x86-64-linux-gnu libc6-dev-amd64-cross" ;; \
      arm64) target=aarch64-unknown-linux-gnu; cross="gcc-aarch64-linux-gnu libc6-dev-arm64-cross" ;; \
      *) echo "Unsupported target architecture: $TARGETARCH" >&2; exit 1 ;; \
    esac; \
    if [ "$TARGETARCH" = "$(dpkg --print-architecture)" ]; then cross=""; fi; \
    apt-get update; \
    apt-get install -y --no-install-recommends gcc libc6-dev $cross; \
    rm -rf /var/lib/apt/lists/*; \
    rustup target add "$target"; \
    printf '%s\n' "$target" > /rust-target

COPY Cargo.toml Cargo.lock ./
COPY src ./src
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/build/target \
    set -eu; \
    target="$(cat /rust-target)"; \
    if [ "$TARGETARCH" != "$(dpkg --print-architecture)" ]; then \
      case "$TARGETARCH" in \
        amd64) export CARGO_TARGET_X86_64_UNKNOWN_LINUX_GNU_LINKER=x86_64-linux-gnu-gcc CC_x86_64_unknown_linux_gnu=x86_64-linux-gnu-gcc ;; \
        arm64) export CARGO_TARGET_AARCH64_UNKNOWN_LINUX_GNU_LINKER=aarch64-linux-gnu-gcc CC_aarch64_unknown_linux_gnu=aarch64-linux-gnu-gcc ;; \
      esac; \
    fi; \
    cargo build --release --locked --target "$target"; \
    install -Dm755 "target/$target/release/watchtower" /out/watchtower

FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates \
    && rm -rf /var/lib/apt/lists/* \
    && mkdir -p /data
COPY --from=build /out/watchtower /usr/local/bin/watchtower
ENV WATCHTOWER_DATA_DIR=/data
EXPOSE 8080
ENTRYPOINT ["watchtower"]
