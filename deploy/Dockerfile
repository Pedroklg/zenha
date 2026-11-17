FROM rust:1.97-slim AS build
WORKDIR /src
COPY . .
RUN cargo build --release --locked --package zenha-server

FROM debian:stable-slim
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates \
    && rm -rf /var/lib/apt/lists/* \
    && groupadd --system --gid 10001 zenha \
    && useradd --system --uid 10001 --gid 10001 --no-create-home --shell /usr/sbin/nologin zenha \
    && install -d -m 0750 -o zenha -g zenha /var/lib/zenha
COPY --from=build /src/target/release/zenha-server /usr/local/bin/zenha-server
USER zenha:zenha
EXPOSE 8787
ENTRYPOINT ["zenha-server"]
