# Fcitx5 addon（wind_linux，C++）。纯逻辑库与单测不依赖 Fcitx5，addon 目标需要 Fcitx5 开发头文件。
#
# 服务 / 设置程序的默认路径由 CMakeLists 的 WIND_SERVICE_PATH / WIND_SETTING_PATH 选项注入
# （PUBLIC 编译定义，单测读同一个值）；运行时仍可用 WIND_INPUT_SERVICE / WIND_INPUT_SETTING
# 覆盖（见 wind_linux/src/core/*Launcher.cpp）。
# addon 版本经 `-DWIND_ADDON_VERSION` 注入（= `docs/VERSION`），写进 fcitx5 addon 描述的
# `Version=`；别让它落回 `project(... VERSION ...)` 的占位值。
# 不套 cmake hook 的 configurePhase：工程在 wind_linux/ 子目录里，直接显式调 cmake 更直白。
# src 收窄到真正被读到的子树 —— 整仓当 src 时，改任何一个文件都会重编 addon 并级联到汇总包：
#   wind_linux/                         工程本体 + 单测读的 data/icons
#   wind_tsf/include/                   wind_linux/include/Protocol.h 直接 include 这里的 BinaryProtocol.h
#   wind_input/crates/{wind-ipc,wind-bridge,wind-ui,wind-keys}/src/
#                                       单测按 WIND_PROTOCOL_RS / WIND_REPO_DIR 读其中的 protocol.rs /
#                                       endpoint.rs / handler.rs / tray_icon.rs / key_inject.rs / keymap.rs
#                                       （留到 src/ 粒度：同 crate 新读一个文件不必回来改这里）
#   wind_macos/Sources/WindInputApp/UI/   keysynth_test 与 macOS 键码表对账用的 KeySynthesizer.swift
{
  lib,
  stdenv,
  cmake,
  ninja,
  pkg-config,
  fcitx5,
  wayland,
  wayland-scanner,
  libxcb,
  glib,
  service,
  setting,
  version,
}:
let
  root = ../.;
in
stdenv.mkDerivation {
  pname = "windinput-fcitx5";
  inherit version;
  src = lib.cleanSourceWith {
    name = "windinput-addon-context";
    src = root;
    filter =
      path: _type:
      let
        rel = lib.removePrefix "${toString root}/" (toString path);
        keep =
          prefix: rel == prefix || lib.hasPrefix "${prefix}/" rel || lib.hasPrefix "${rel}/" prefix;
      in
      rel == "" || lib.any keep [
        "wind_linux"
        "wind_tsf/include"
        "wind_input/crates/wind-ipc/src"
        "wind_input/crates/wind-bridge/src"
        "wind_input/crates/wind-ui/src"
        "wind_input/crates/wind-keys/src"
        "wind_macos/Sources/WindInputApp/UI"
      ];
  };

  nativeBuildInputs = [
    cmake
    ninja
    pkg-config
    wayland-scanner
  ];
  buildInputs = [
    fcitx5
    wayland
    libxcb
    glib
  ];

  configurePhase = ''
    runHook preConfigure
    cmake -S wind_linux -B build -G Ninja \
      -DCMAKE_BUILD_TYPE=Release \
      -DCMAKE_INSTALL_PREFIX=$out \
      -DCMAKE_INSTALL_LIBDIR=lib \
      -DCMAKE_INSTALL_DATADIR=share \
      -DWIND_BUILD_ADDON=ON \
      -DWIND_SERVICE_PATH="${service}/lib/windinput/wind_input" \
      -DWIND_ADDON_VERSION="${version}" \
      -DWIND_SETTING_PATH="${setting}/lib/windinput/wind_setting"
    runHook postConfigure
  '';

  buildPhase = ''
    runHook preBuild
    cmake --build build --parallel
    runHook postBuild
  '';

  # 纯逻辑单测（不需要 Fcitx5 运行时）。
  doCheck = true;
  checkPhase = ''
    runHook preCheck
    ctest --test-dir build --output-on-failure
    # 单测只验「路径形状」（绝对路径 + 末段名），宏未定义时源码内置的 /usr/lib/windinput/…
    # 同样满足 —— 注入没生效时它们照样全绿，运行时却拉不起程序。这里直接验产物：
    # 两条注入路径的 store 前缀必须原样出现在 addon 里（strip 发生在 installPhase 之后）。
    # 只比到 `lib/windinput/`：GCC 会把 `wind_input` 尾部与前面的 `windinput` 做共享后缀
    # 合并，.rodata 里整串不连续，再往后比会假失败。
    grep -aFq -- '${service}/lib/windinput/' build/libwindinput.so
    grep -aFq -- '${setting}/lib/windinput/' build/libwindinput.so
    runHook postCheck
  '';

  installPhase = ''
    runHook preInstall
    DESTDIR= cmake --install build --prefix $out
    runHook postInstall
  '';

  meta = {
    description = "清风输入法 Fcitx5 addon";
    homepage = "https://windinput.com";
    license = lib.licenses.gpl3Plus;
    platforms = lib.platforms.linux;
  };
}
