# Builder stage
FROM ubuntu:24.04 AS builder

# Prevent interactive prompts
ENV DEBIAN_FRONTEND=noninteractive

# Install build dependencies
RUN apt-get update && apt-get install -y \
    curl \
    build-essential \
    pkg-config \
    libssl-dev \
    cmake \
    g++ \
    libvulkan-dev \
    libcurl4-openssl-dev \
    zlib1g-dev \
    && rm -rf /var/lib/apt/lists/*

# Install Rust manually since we're using Ubuntu base
RUN curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y
ENV PATH="/root/.cargo/bin:${PATH}"

WORKDIR /app
COPY . .

# Build the project
RUN cargo build --release

# Runner stage
FROM ubuntu:24.04

# Prevent interactive prompts
ENV DEBIAN_FRONTEND=noninteractive

# Install runtime dependencies
RUN apt-get update && apt-get install -y \
    ca-certificates \
    libvulkan1 \
    libssl3 \
    libcurl4 \
    libstdc++6 \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /app

# Copy binary
COPY --from=builder /app/target/release/rsts /app/rsts
COPY rsts.example.toml /app/rsts.example.toml

# Create data directories
RUN mkdir -p data/templates data/cache data/styles data/sprites data/mbtiles data/fonts

# Expose port
EXPOSE 3001

CMD ["./rsts"]
