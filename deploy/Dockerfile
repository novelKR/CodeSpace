# Unprivileged Linux runner. Do not mount host home, SSH agent sockets,
# the engine socket, or gateway secrets into this image.

FROM debian:bookworm-slim

RUN useradd --uid 10001 --create-home --shell /usr/sbin/nologin codespace \
    && mkdir -p /workspace \
    && chown 10001:10001 /workspace

USER 10001:10001
WORKDIR /workspace

# The gateway binary is not copied here. Commands run against /workspace only.
CMD ["sleep", "infinity"]
