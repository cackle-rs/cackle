# Utility Docker image with cargo-acl installed, to assist macos users.
FROM rust:latest AS builder

WORKDIR /build
COPY . .

# Install cackle from local path
RUN cargo install --path .

# Create app user
ENV USER=web
ENV UID=1001

RUN useradd \
    --home "/nonexistent" \
    --shell "/sbin/nologin" \
    --no-create-home \
    --uid "${UID}" \
    "${USER}"

# Runtime stage
FROM rust:latest AS runtime
#checkov:skip=CKV_DOCKER_2:"Ensure that HEALTHCHECK instructions have been added to container images"

# Install bubblewrap for cackle sandbox
RUN apt-get update && \
    apt-get install -y --no-install-recommends bubblewrap ca-certificates cmake && \
    apt-get clean && \
    rm -rf /var/lib/apt/lists/*

# Copy users and groups from builder
COPY --from=builder /etc/passwd /etc/passwd
COPY --from=builder /etc/group /etc/group

# Copy cargo-acl binary from builder
COPY --from=builder /usr/local/cargo/bin/cargo-acl /usr/local/cargo/bin/cargo-acl

# Set user and group
USER web:web

WORKDIR /workspace

CMD ["cargo", "acl"]
