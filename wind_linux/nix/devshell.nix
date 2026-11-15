# 开发 shell：与 scripts/linux/README.md 列的依赖对齐（Fcitx5 开发头文件 + xcb + wayland + gio）。
# 进入后可直接：
#   cargo build --release -p wind_service --features linux-host   (在 wind_input/ 下)
#   cmake -S wind_linux -B wind_linux/build -G Ninja && cmake --build wind_linux/build
{
  lib,
  mkShell,
  rustc,
  cargo,
  rustfmt,
  clippy,
  cmake,
  ninja,
  pkg-config,
  fcitx5,
  wayland,
  wayland-scanner,
  libxcb,
  glib,
  curl,
  git,
}:
mkShell {
  packages = [
    rustc
    cargo
    rustfmt
    clippy
    cmake
    ninja
    pkg-config
    fcitx5
    wayland
    wayland-scanner
    libxcb
    glib
    curl
    git
  ];

  # shellHook 只打印提示。addon 构建要的模块（fcitx5 / xcb / wayland / glib）能被
  # `pkg_check_modules` 找到，是因为它们与 pkg-config 一同进了 shell 环境，由 pkg-config 的
  # setup hook 自动接线 PKG_CONFIG_PATH —— 与 shellHook 无关，别指望它做别的事。
  shellHook = ''
    echo "WindInput devShell：cargo / cmake / fcitx5 头文件已就绪"
  '';
}
