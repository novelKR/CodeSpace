# Isolation fixture. Do not mount host home, SSH agent sockets,
# Docker engine sockets, or gateway secrets into this image.
# There is no runner control socket today.

FROM debian:bookworm-slim

RUN useradd --uid 10001 --create-home --shell /usr/sbin/nologin codespace \
    && mkdir -p /workspace \
    && chown 10001:10001 /workspace

USER 10001:10001
WORKDIR /workspace

# The gateway binary is not copied here. This image only holds /workspace.
CMD ["sleep", "infinity"]
