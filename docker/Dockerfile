# Stage 1: Build Rezolus from source
FROM debian:bookworm AS builder

ENV DEBIAN_FRONTEND=noninteractive

RUN apt-get update && apt-get install -y \
    build-essential \
    pkg-config \
    libelf-dev \
    clang \
    curl \
    && rm -rf /var/lib/apt/lists/*

# Install Rust toolchain
RUN curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y
ENV PATH="/root/.cargo/bin:${PATH}"

WORKDIR /build

# Copy source tree
COPY . .

# Build release binary
RUN cargo build --release

# Stage 2: Runtime image
FROM debian:bookworm-slim

ENV DEBIAN_FRONTEND=noninteractive

RUN apt-get update && apt-get install -y \
    libelf1 \
    curl \
    && rm -rf /var/lib/apt/lists/*

# Copy the rezolus binary
COPY --from=builder /build/target/release/rezolus /usr/local/bin/rezolus

# Copy default agent config
COPY config/agent.toml /etc/rezolus/agent.toml

# Copy entrypoint and capture scripts
COPY docker/entrypoint.sh /usr/local/bin/entrypoint.sh
COPY docker/rezolus-capture /usr/local/bin/rezolus-capture
RUN chmod +x /usr/local/bin/entrypoint.sh /usr/local/bin/rezolus-capture

# Create default output directory
RUN mkdir -p /data

# Agent port
EXPOSE 4241
# Viewer port
EXPOSE 8080

ENTRYPOINT ["/usr/local/bin/entrypoint.sh"]
