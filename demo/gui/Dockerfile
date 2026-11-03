# The image the GUI screenshots are taken in (`cargo xtask demo --gui`): an X server with
# software rendering (Mesa's llvmpipe and lavapipe), FUSE for dfdu-demofs, and libfaketime.
# Debian trixie, as the rust image the binaries are built in, so they find the glibc they were
# linked against. dfdu-gui links no X11, GL or Vulkan library; it loads them when it starts.
FROM debian:trixie-slim
RUN apt-get update \
    && apt-get install -y --no-install-recommends \
        xvfb xdotool netpbm fuse3 faketime \
        libgl1 libegl1 libgl1-mesa-dri mesa-vulkan-drivers libvulkan1 \
        libx11-6 libx11-xcb1 libxcursor1 libxrandr2 libxi6 libxkbcommon0 libxkbcommon-x11-0 \
    && rm -rf /var/lib/apt/lists/* \
    && ln -s /usr/lib/*/faketime/libfaketime.so.1 /usr/local/lib/libfaketime.so.1 \
    && test -e /usr/local/lib/libfaketime.so.1 \
    && mkdir -p /opt/dfdu/bin /work
ENV PATH=/opt/dfdu/bin:$PATH
WORKDIR /work
ENTRYPOINT ["/work/gui/run.sh"]
