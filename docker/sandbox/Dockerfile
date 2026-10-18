# Gagent sandbox image.
#
# A compact Ubuntu environment in which the agent runs the code it writes.
# Gagent builds this image automatically the first time it is needed. To build
# it yourself (for example to make a smaller one):
#
#     docker build -t gagent-sandbox:latest docker/sandbox
#     docker build -t gagent-sandbox:latest --build-arg WITH_NODE=0 --build-arg WITH_CPP=0 docker/sandbox
#
# Build arguments:
#     BASE_IMAGE  base image, e.g. a registry mirror of ubuntu:24.04
#     WITH_CPP    1 = include g++ (C++ projects); 0 = C only
#     WITH_NODE   1 = include Node.js and npm; 0 = leave them out
ARG BASE_IMAGE=ubuntu:24.04
FROM ${BASE_IMAGE}

ARG WITH_CPP=1
ARG WITH_NODE=1

LABEL org.opencontainers.image.title="gagent-sandbox" \
      org.opencontainers.image.description="Sandbox environment for the Gagent desktop agent" \
      org.opencontainers.image.source="https://github.com/layer8labs8/Gagent"

ENV DEBIAN_FRONTEND=noninteractive \
    LANG=C.UTF-8 \
    LC_ALL=C.UTF-8

# Python 3, a C compiler, git and common command line tools. Node.js and g++ are optional (see above).
RUN set -eux; \
    packages="bash ca-certificates curl file git gcc jq libc6-dev make patch procps \
              python3 python3-pip python3-venv unzip xz-utils zip"; \
    if [ "$WITH_CPP" = "1" ]; then packages="$packages g++"; fi; \
    if [ "$WITH_NODE" = "1" ]; then packages="$packages nodejs npm"; fi; \
    apt-get update; \
    apt-get install -y --no-install-recommends $packages; \
    rm -rf /var/lib/apt/lists/*; \
    # The workspace is bind-mounted and owned by a different user than the one inside the container.
    git config --system --add safe.directory '*'

WORKDIR /workspace
CMD ["sleep", "infinity"]
