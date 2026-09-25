# The Linux release build environment, used by scripts/linux-package.sh.
#
# A binary links against the glibc symbol versions of the machine that built
# it and refuses to start anywhere older, so releases are built here rather
# than on the host. Ubuntu 22.04 is glibc 2.35: old enough to cover Ubuntu
# 22.04+, Debian 12+, Fedora 36+ and RHEL 9, recent enough that a current
# Rust still installs cleanly. linux-package.sh checks the result against
# the same floor (GLIBC_FLOOR there); change the two together.
FROM ubuntu:22.04

ENV DEBIAN_FRONTEND=noninteractive \
    RUSTUP_HOME=/usr/local/rustup \
    PATH=/usr/local/cargo/bin:$PATH

# Same list as .github/workflows/ci.yml, minus the shells the tests need.
RUN apt-get update && apt-get install -y --no-install-recommends \
      build-essential pkg-config curl ca-certificates git \
      libfontconfig1-dev libfreetype6-dev \
      libwayland-dev libxkbcommon-dev libxkbcommon-x11-dev \
      libx11-dev libxcb1-dev libxcb-render0-dev libxcb-shape0-dev \
      libxcb-xfixes0-dev libssl-dev \
    && rm -rf /var/lib/apt/lists/*

# Edition 2024 needs Rust 1.85+ and 22.04 packages 1.66, so rustup. Pinned so
# the image builds the same thing every time; bump it with the host toolchain.
# The build runs as the invoking user (not root), hence the chmod: rustup
# touches its own dir even when nothing needs installing. CARGO_HOME is set
# at run time to a bind mount, so the registry cache outlives the container.
ARG RUST_VERSION=1.97.1
RUN curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs \
      | CARGO_HOME=/usr/local/cargo sh -s -- -y --no-modify-path \
          --default-toolchain "$RUST_VERSION" --profile minimal \
    && chmod -R a+w /usr/local/rustup

WORKDIR /src
