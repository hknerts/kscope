# Build stage
FROM rust:1-bookworm AS builder
WORKDIR /src

# Cache dependencies separately from the source tree.
COPY Cargo.toml Cargo.lock ./
RUN mkdir -p src && \
    echo 'fn main() {}' > src/main.rs && \
    echo '' > src/lib.rs && \
    cargo build --release --locked && \
    rm -rf src

COPY . .
RUN touch src/main.rs src/lib.rs && cargo build --release --locked

# Runtime stage: distroless, non-root, no shell.
FROM gcr.io/distroless/cc-debian12:nonroot
COPY --from=builder /src/target/release/kscope /usr/local/bin/kscope
USER nonroot
ENV TERM=xterm-256color
ENTRYPOINT ["/usr/local/bin/kscope"]
