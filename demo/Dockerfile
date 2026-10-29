# The image the demo recordings are made in: VHS with libfaketime and a `demo` user. The
# binaries, tapes and fixtures are copied into each container (`cargo xtask demo`), so nothing
# of the host is mounted into it.
# VHS v0.12.1, pinned so the recordings do not change with a new image.
FROM ghcr.io/charmbracelet/vhs:v0.12.1@sha256:ea49a6a1c529be83153e88321892b5585964418f0b9055e8c1e0d732194234a3
RUN apt-get update \
    && apt-get install -y --no-install-recommends faketime \
    && rm -rf /var/lib/apt/lists/* \
    && ln -s /usr/lib/*/faketime/libfaketime.so.1 /usr/local/lib/libfaketime.so.1 \
    && test -e /usr/local/lib/libfaketime.so.1 \
    && useradd --uid 1000 --create-home --shell /bin/bash demo \
    && mkdir -p /opt/dfdu/bin /work
ENV PATH=/opt/dfdu/bin:$PATH
WORKDIR /work
ENTRYPOINT ["/work/run.sh"]
