# Ubuntu 22.04 构建环境：产物要能在 22.04 上运行（glibc 2.35、fcitx5 5.0.x），
# 在更新的发行版上构建会引入 GLIBC_2.38+ 符号和更新的 fcitx5 ABI，拿到 22.04 上加载不起来。
# 用法见 scripts/linux/package-deb.sh。
FROM ubuntu:22.04
ARG RUST_VERSION=stable
ENV DEBIAN_FRONTEND=noninteractive
RUN apt-get update && apt-get install -y --no-install-recommends \
        build-essential cmake ninja-build pkg-config curl ca-certificates git dpkg-dev \
        libfcitx5core-dev libfcitx5utils-dev libfcitx5config-dev fcitx5-modules-dev \
        libxcb1-dev libxcb-shape0-dev libxcb-randr0-dev libwayland-dev libwayland-bin libglib2.0-dev \
    && rm -rf /var/lib/apt/lists/*
ENV RUSTUP_HOME=/opt/rustup CARGO_HOME=/opt/cargo PATH=/opt/cargo/bin:$PATH
RUN curl -fsSL https://sh.rustup.rs | sh -s -- -y --profile minimal --default-toolchain ${RUST_VERSION} \
    && chmod -R a+rX /opt/rustup /opt/cargo
