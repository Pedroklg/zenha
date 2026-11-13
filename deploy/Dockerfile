FROM rust:1.97-slim AS build
WORKDIR /src
COPY . .
RUN cargo build --release --package rooms-token-server

FROM debian:stable-slim
COPY --from=build /src/target/release/rooms-token-server /usr/local/bin/
ENTRYPOINT ["rooms-token-server"]
