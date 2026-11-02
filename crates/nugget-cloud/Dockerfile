# Build from the repository root:  docker build -f crates/nugget-cloud/Dockerfile -t nugget-cloud .
FROM rust:1-bookworm AS build
WORKDIR /src
COPY . .
RUN cargo build --release --locked -p nugget-cloud

FROM debian:bookworm-slim
# ca-certificates: the OS trust store for the AI, Composio and Supabase HTTPS endpoints.
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates \
    && rm -rf /var/lib/apt/lists/* && useradd --system --uid 10001 nugget
COPY --from=build /src/target/release/nugget-cloud /usr/local/bin/nugget-cloud
# Supabase signs its database certificates with its own root CA; download it from
# Project Settings → Database → SSL and ship it as a Fly secret file, or COPY it here.
USER nugget
ENV NUGGET_CLOUD_BIND=0.0.0.0:8080
EXPOSE 8080
CMD ["nugget-cloud"]
