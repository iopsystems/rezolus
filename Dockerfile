FROM --platform=$BUILDPLATFORM rust:1 AS builder

# Install cross-compilation tools and libraries for both architectures
RUN dpkg --add-architecture arm64 && \
    apt-get update && apt-get install -y \
    clang \
    build-essential \
    gcc-aarch64-linux-gnu \
    libelf-dev \
    zlib1g-dev \
    libbpf-dev \
    libelf-dev:arm64 \
    zlib1g-dev:arm64 \
    libbpf-dev:arm64 \
    && rm -rf /var/lib/apt/lists/*

# Set up working directory
WORKDIR /app

# Copy Cargo files
COPY . .

RUN rustup target add x86_64-unknown-linux-gnu && \
    rustup target add aarch64-unknown-linux-gnu && \
    rustup component add rustfmt

# Define build argument for target platform
ARG TARGETPLATFORM

# Set up cross-compilation environment
RUN case "$TARGETPLATFORM" in \
    "linux/amd64") \
    export CARGO_TARGET_X86_64_UNKNOWN_LINUX_GNU_LINKER=x86_64-linux-gnu-gcc &&\
    export PKG_CONFIG_PATH=/usr/lib/x86_64-linux-gnu/pkgconfig &&\
    cargo build --release --target x86_64-unknown-linux-gnu && \
    cp target/x86_64-unknown-linux-gnu/release/rezolus /app/rezolus \
    ;; \
    "linux/arm64") \
    export CARGO_TARGET_AARCH64_UNKNOWN_LINUX_GNU_LINKER=aarch64-linux-gnu-gcc &&\
    export PKG_CONFIG_PATH=/usr/lib/aarch64-linux-gnu/pkgconfig &&\
    cargo build --release --target aarch64-unknown-linux-gnu && \
    cp target/aarch64-unknown-linux-gnu/release/rezolus /app/rezolus \
    ;; \
    esac

# Final stage: create minimal runtime image
FROM debian:bookworm-slim

# Install runtime dependencies
RUN dpkg --add-architecture arm64 && \
    apt-get update && apt-get install -y \
    ca-certificates \
    libelf1 \
    zlib1g \
    libbpf-dev \
    libelf1:arm64 \
    zlib1g:arm64 \
    libbpf-dev:arm64 \
    && rm -rf /var/lib/apt/lists/*

# Copy the built binary
COPY --from=builder /app/rezolus /usr/local/bin/rezolus

# Set the entrypoint
ENTRYPOINT ["/usr/local/bin/rezolus"]
