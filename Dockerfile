# syntax=docker/dockerfile:1
#
# The build stage always runs on the builder's own architecture and
# cross-compiles for the target one. Building the arm64 image under QEMU
# emulation took over half an hour; cross-compiling takes a few minutes, and
# links against the same Debian 12 glibc the runtime image ships.
FROM --platform=$BUILDPLATFORM rust:1-bookworm AS builder
ARG BUILDARCH
ARG TARGETARCH
WORKDIR /src

RUN set -eu; \
    case "$TARGETARCH" in \
      amd64) triple=x86_64-unknown-linux-gnu; gcc=x86-64-linux-gnu ;; \
      arm64) triple=aarch64-unknown-linux-gnu; gcc=aarch64-linux-gnu ;; \
      *) echo "unsupported architecture: $TARGETARCH" >&2; exit 1 ;; \
    esac; \
    echo "$triple" > /triple; \
    rustup target add "$triple"; \
    if [ "$TARGETARCH" != "$BUILDARCH" ]; then \
      apt-get update && apt-get install -y --no-install-recommends "gcc-$gcc" "libc6-dev-$TARGETARCH-cross"; \
      rm -rf /var/lib/apt/lists/*; \
    fi
ENV CARGO_TARGET_AARCH64_UNKNOWN_LINUX_GNU_LINKER=aarch64-linux-gnu-gcc \
    CC_aarch64_unknown_linux_gnu=aarch64-linux-gnu-gcc \
    CARGO_TARGET_X86_64_UNKNOWN_LINUX_GNU_LINKER=x86_64-linux-gnu-gcc \
    CC_x86_64_unknown_linux_gnu=x86_64-linux-gnu-gcc

# Cache dependencies separately from the source tree.
COPY Cargo.toml Cargo.lock ./
RUN mkdir -p src && \
    echo 'fn main() {}' > src/main.rs && \
    echo '' > src/lib.rs && \
    cargo build --release --locked --target "$(cat /triple)" && \
    rm -rf src

COPY . .
RUN touch src/main.rs src/lib.rs && \
    cargo build --release --locked --target "$(cat /triple)" && \
    cp "target/$(cat /triple)/release/kscope" /kscope

# Runtime stage: distroless, non-root, no shell.
FROM gcr.io/distroless/cc-debian12:nonroot
COPY --from=builder /kscope /usr/local/bin/kscope
USER nonroot
ENV TERM=xterm-256color
ENTRYPOINT ["/usr/local/bin/kscope"]
