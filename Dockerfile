# The smartplay engine as a container image.
#
#   docker build -t smartplay-engine .
#
# The plugin is not part of the image: it is a file Navidrome loads (see deploy/README.md).

# Build on Debian 12, the same release as the final image, so the glibc the program is linked against is the one it runs on.
FROM rust:1-bookworm AS build
WORKDIR /src
COPY . .
# The workspace also holds the plugin crate, which depends on Navidrome's plugin SDK by path: cargo needs it in place to
# read the workspace, even though only the engine is built here.
RUN ./scripts/fetch-deps.sh \
 && cargo build --release --locked -p smartplay-engine

FROM debian:bookworm-slim
LABEL org.opencontainers.image.source="https://github.com/Ulises961/smartplay" \
      org.opencontainers.image.licenses="GPL-3.0-or-later" \
      org.opencontainers.image.description="The smartplay engine: playlists for the listeners of a Subsonic-compatible music server"
# The engine's state (libraries, plays, decisions, tokens) lives in /state. The user matches the one the compose example
# runs as, so a state folder made for it needs no change.
RUN useradd --uid 1000 --no-create-home --shell /usr/sbin/nologin smartplay \
 && mkdir /state && chown smartplay /state
COPY --from=build /src/target/release/smartplay-engine /usr/local/bin/smartplay-engine
USER smartplay
VOLUME /state
EXPOSE 7788
ENTRYPOINT ["smartplay-engine"]
CMD ["serve", "--bind", "0.0.0.0:7788", "--data", "/state"]
