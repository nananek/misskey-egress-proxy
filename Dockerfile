# SPDX-FileCopyrightText: misskey-egress-proxy contributors
# SPDX-License-Identifier: AGPL-3.0-only

FROM rust:1-slim-bookworm AS build
WORKDIR /build
COPY Cargo.toml Cargo.lock ./
COPY src ./src
RUN cargo build --release --locked

FROM gcr.io/distroless/cc-debian12:nonroot
LABEL org.opencontainers.image.source="https://github.com/nananek/misskey-egress-proxy"
LABEL org.opencontainers.image.description="Minimal ActivityPub-federation-only reverse proxy for Misskey"
LABEL org.opencontainers.image.licenses="AGPL-3.0-only"
COPY --from=build /build/target/release/misskey-egress-proxy /usr/local/bin/misskey-egress-proxy
USER nonroot
ENTRYPOINT ["/usr/local/bin/misskey-egress-proxy"]
