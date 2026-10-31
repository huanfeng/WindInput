# 图形设置程序（独立仓库 wind-setting）。
#
# 它经 path 依赖引用两个兄弟仓：
#   ../WindInput/wind_input/crates/{wind-ipc,wind-rpc,wind-config,wind-punct,wind-transform}
#   ../wind-ui-rust
# 并且 `src/xdg_assoc.rs` 用 `include_str!` 内嵌主仓的桌面集成入口：
#   ../../WindInput/scripts/linux/pkg/windinput-{setting,import}.desktop、windinput-mime.xml
# 因此先把三份源码拼成 deb 构建时同样的兄弟布局，再在其上构建。
#
# 主仓只取这两处子树，其余（`docs/`、`data/`、`wind_linux/` …）连派生输入都不进：
# 过滤发生在 `cleanSourceWith`，无关文件的改动不改变本派生的输入哈希，设置程序不会重编。
# 子树内再剔掉构建产物（`wind_input/target/`，见 `src-filter.nix`）：本机 cargo 产物不是
# 源码，进派生输入会让 `path:` 下每轮构建都换一个哈希。
# 不进一步裁到「path 依赖子集」：那些 crate 属于 wind_input 的 workspace，
# members 清单在根 Cargo.toml 里，裁 crate 就得改写 members——新增一条 path 依赖时
# 会以「找不到 workspace member」的面目失败，代价大于省下的那点重编。
#
# cargo 的 setup hook 只在源码根找 Cargo.lock，而仓库根（兄弟布局的父目录）没有；
# 把 wind-setting 的锁文件复制到根，再在 preBuild 里切进 wind-setting 目录。
{
  lib,
  stdenvNoCC,
  rustPlatform,
  makeBinaryWrapper,
  fontconfig,
  version,
  windInputSrc,
  windSettingSrc,
  windUiRustSrc,
}:
let
  srcFilter = import ./src-filter.nix { inherit lib; };

  # 设置程序用到的两处主仓路径（`scripts/linux/pkg/` 整目录拷：`include_str!` 少一份就编不过，
  # 整目录可让该目录新增桌面入口时不必回来改这里）。
  wantedPrefixes = [
    "wind_input"
    "scripts/linux/pkg"
  ];

  windInputSubset = lib.cleanSourceWith {
    name = "windinput-setting-context";
    src = windInputSrc;
    filter =
      path: _type:
      let
        rel = lib.removePrefix "${toString windInputSrc}/" (toString path);
        keep =
          prefix: rel == prefix || lib.hasPrefix "${prefix}/" rel || lib.hasPrefix "${rel}/" prefix;
      in
      rel == "" || (lib.any keep wantedPrefixes && !srcFilter.isBuildOutput rel);
  };

  # 只拷贝/布局一棵源码树，不编任何 C —— stdenv 会把 gcc/binutils 整套工具链拉进构建输入。
  srcTree = stdenvNoCC.mkDerivation {
    pname = "windinput-setting-src";
    inherit version;
    dontUnpack = true;
    buildCommand = ''
      mkdir -p $out/WindInput/scripts/linux/pkg
      cp -r ${windInputSubset}/wind_input $out/WindInput/wind_input
      cp ${windInputSubset}/scripts/linux/pkg/* $out/WindInput/scripts/linux/pkg/
      cp -r ${windSettingSrc} $out/wind-setting
      cp -r ${windUiRustSrc} $out/wind-ui-rust
      cp ${windSettingSrc}/Cargo.lock $out/Cargo.lock
      chmod -R u+w $out
    '';
  };
in
rustPlatform.buildRustPackage {
  pname = "wind_setting";
  inherit version;
  src = srcTree;
  cargoLock.lockFile = "${windSettingSrc}/Cargo.lock";
  preBuild = "cd wind-setting";

  # 与服务同一套文字栈：`windui` 运行期 `dlopen("libfontconfig.so.1")` 选字，ELF 里没有
  # NEEDED。不注入 LD_LIBRARY_PATH 的话 NixOS 上（无 ldconfig 缓存）只能退回扫描字体目录，
  # 设置界面与候选窗的选字/字重就会分成两条路径。机制说明见 `service.nix`。
  nativeBuildInputs = [ makeBinaryWrapper ];
  buildInputs = [ (lib.getLib fontconfig) ];

  # build.rs 优先读 WIND_APP_VERSION（与 core / 安装包同一份 docs/VERSION）。
  env.WIND_APP_VERSION = version;

  doCheck = false;

  installPhase = ''
    runHook preInstall
    bin=$(find target -type f -name wind_setting -path '*/release/*' -print -quit)
    [ -n "$bin" ] || { echo "找不到 wind_setting 构建产物" >&2; exit 1; }
    mkdir -p $out/lib/windinput
    install -Dm755 "$bin" $out/lib/windinput/wind_setting
    runHook postInstall
  '';

  postInstall = ''
    wrapProgram $out/lib/windinput/wind_setting \
      --prefix LD_LIBRARY_PATH : ${lib.makeLibraryPath [ (lib.getLib fontconfig) ]}
  '';

  meta = {
    description = "清风输入法图形设置程序";
    homepage = "https://windinput.com";
    license = lib.licenses.gpl3Plus;
    platforms = lib.platforms.linux;
  };
}
