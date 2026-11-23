# Card Benefits Tracker: one image with the Rust API and the built Angular app.
#
# Build on the Pi itself:       podman build -t card-benefits .
# Cross-build for a 64-bit Pi:  podman build --platform linux/arm64 -t card-benefits .
#
# The build stages always run on the build machine's own architecture and
# cross-compile, so an arm64 image builds on an x86 laptop without emulation.

FROM --platform=$BUILDPLATFORM docker.io/oven/bun:1 AS bun

FROM --platform=$BUILDPLATFORM docker.io/library/node:22-bookworm-slim AS web
COPY --from=bun /usr/local/bin/bun /usr/local/bin/bun
WORKDIR /src/frontend
COPY frontend/package.json frontend/bun.lock ./
RUN bun install --frozen-lockfile
COPY frontend/ ./
RUN bun run build

FROM --platform=$BUILDPLATFORM docker.io/library/rust:1-bookworm AS build
ARG TARGETARCH
WORKDIR /src/backend
RUN set -eux; \
    case "$TARGETARCH" in \
      arm64) triple=aarch64-unknown-linux-gnu; gnu=aarch64-linux-gnu ;; \
      amd64) triple=x86_64-unknown-linux-gnu;  gnu=x86_64-linux-gnu ;; \
      *) echo "unsupported architecture: $TARGETARCH (a 64-bit OS is required)"; exit 1 ;; \
    esac; \
    echo "$triple" > /triple; \
    rustup target add "$triple"; \
    if [ "$(dpkg --print-architecture)" != "$TARGETARCH" ]; then \
      apt-get update; \
      apt-get install -y --no-install-recommends "gcc-$(echo "$gnu" | tr _ -)" "libc6-dev-$TARGETARCH-cross"; \
      rm -rf /var/lib/apt/lists/*; \
      mkdir -p /usr/local/cargo; \
      printf '[target.%s]\nlinker = "%s-gcc"\n' "$triple" "$gnu" >> /usr/local/cargo/config.toml; \
      echo "export CC_$(echo "$triple" | tr - _)=$gnu-gcc" > /cross-env; \
    else \
      : > /cross-env; \
    fi
# Dependencies first, so source edits do not rebuild them.
COPY backend/Cargo.toml backend/Cargo.lock ./
RUN mkdir src && echo 'fn main() {}' > src/main.rs && touch src/lib.rs \
    && . /cross-env && cargo build --release --locked --target "$(cat /triple)" \
    && rm -rf src
COPY backend/ ./
RUN . /cross-env && touch src/main.rs src/lib.rs \
    && cargo build --release --locked --target "$(cat /triple)" \
    && cp "target/$(cat /triple)/release/card-benefits" /card-benefits

# No RUN steps below: the runtime stage is only assembled, never executed,
# during a cross-build.
FROM gcr.io/distroless/cc-debian12:nonroot
WORKDIR /app
COPY --from=build /card-benefits /app/card-benefits
COPY --from=web /src/frontend/dist/frontend/browser /app/static
COPY catalog /app/catalog
ENV BIND_ADDR=0.0.0.0:8080 STATIC_DIR=/app/static CATALOG_DIR=/app/catalog
EXPOSE 8080
USER nonroot
ENTRYPOINT ["/app/card-benefits"]
