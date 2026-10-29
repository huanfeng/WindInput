# Rust 核心服务（wind_input，Linux 宿主形态）。
#
# 关键约束：服务按 `current_exe()` 的同级目录找 `data/`（wind-config::variant::install_root），
# 所以二进制与数据必须落在同一个真实目录里 —— 这里直接装成 `$out/lib/windinput/{wind_input,data}`，
# 不做软链。addon 在编译期就被钉到本包的 `lib/windinput/wind_input`，因此包自身必须自足。
#
# `$out/bin/wind_input` 是给 Nix 的程序接口用的**相对软链**（`meta.mainProgram` 按
# `$out/bin/<名>` 解析，`nix run` / `lib.getExe` 走它），指向上面那个真实文件：内核经
# `/proc/self/exe` 解析到真实路径，`install_root` 仍是 `$out/lib/windinput`（`data/` 在那儿），
# 故不影响上面那条约束。
{
  lib,
  makeBinaryWrapper,
  rustPlatform,
  fontconfig,
  version,
  rev ? "unknown",
  sourceDateEpoch ? 315532800,
  data ? null,
}:
let
  srcFilter = import ./src-filter.nix { inherit lib; };
in
rustPlatform.buildRustPackage {
  pname = "windinput";
  inherit version;

  # 可复现：版本戳取 `sourceDateEpoch`（flake.nix 传 flake 源的 `lastModified`；git 源是
  # 提交时间，`path:` 源是源码树 mtime）。stdenv 自带的只是兜底常量 315532800（1980-01-01），
  # **不是**源码 mtime —— 不显式设置就会让二进制里的 build 恒为 1980。build.rs 对畸形值
  # 直接报错，不会退回当前时刻。
  # 沙箱里没有 `.git`，`git_describe()` 只会得到 unknown —— 把 flake 的 revision 直接交给
  # build.rs（`WIND_GIT_HASH_OVERRIDE`，见 apps/service/build.rs::emit_build_stamp）。
  env.SOURCE_DATE_EPOCH = toString sourceDateEpoch;
  env.WIND_GIT_HASH_OVERRIDE = rev;

  # 运行时经 dlopen("libfontconfig.so.1") 选字（wind-ui/text/linux/fontconfig.rs；ELF 里没有
  # NEEDED）。deb 把 libfontconfig1 列为硬依赖，同一理由。
  # 实测：`runtimeDependencies` 在这里不成立 —— cc-wrapper 会按 buildInputs 把 fontconfig 写进
  # RUNPATH，但紧随的 stdenv `patchelf --shrink-rpath` 会把它当「未使用」条目剪掉（ELF 里没有
  # NEEDED 引用它），结果 dlopen 仍失败。改用 wrapper 注入 LD_LIBRARY_PATH，并让 buildInputs
  # 把 lib 输出带进闭包。
  nativeBuildInputs = [ makeBinaryWrapper ];
  buildInputs = [ (lib.getLib fontconfig) ];

  postInstall = ''
    wrapProgram $out/lib/windinput/wind_input \
      --prefix LD_LIBRARY_PATH : ${lib.makeLibraryPath [ (lib.getLib fontconfig) ]}
  '';

  # 以 workspace 根为 src：cargo 的 setup hook 只在源码根找 Cargo.lock。
  # 只剔 `target/`（见 `src-filter.nix`）：`path:` 源下本机 cargo 产物否则会成为派生输入，
  # 每轮构建都换一个 store 路径并级联重建四层。
  src = srcFilter.withoutBuildOutput ../wind_input;
  cargoLock.lockFile = ../wind_input/Cargo.lock;

  # 三处编译期读取仓库根，源码树只带 wind_input/ 时补到构建根：
  #   - apps/service/build.rs：../../..//docs/VERSION（产品版本）
  #   - crates/wind-coordinator/build.rs：同一份 docs/VERSION（「关于」菜单）
  #   - wind-theme：include_str!("../../../../data/themes/_base/theme.toml")
  postPatch = ''
    mkdir -p "$NIX_BUILD_TOP/docs" "$NIX_BUILD_TOP/data/themes/_base"
    printf '%s\n' '${version}' > "$NIX_BUILD_TOP/docs/VERSION"
    cp ${../data/themes/_base/theme.toml} "$NIX_BUILD_TOP/data/themes/_base/theme.toml"
  '';

  # 只构建服务二进制；linux-host 关掉桌面渲染路径（外部宿主 = Fcitx5 addon）。
  cargoBuildFlags = [ "-p" "wind_service" ];
  buildFeatures = [ "linux-host" ];

  # 单元测试需要词库数据与宿主环境，打包阶段不跑（CI 已有专门的测试门）。
  doCheck = false;

  installPhase = ''
    runHook preInstall
    bin=$(find target -type f -name wind_input -path '*/release/*' -print -quit)
    [ -n "$bin" ] || { echo "找不到 wind_input 构建产物" >&2; exit 1; }
    mkdir -p $out/lib/windinput
    install -Dm755 "$bin" $out/lib/windinput/wind_input
    ${lib.optionalString (data != null) ''
      cp -r ${data} $out/lib/windinput/data
    ''}
    # 程序入口（`meta.mainProgram`）：只放一条相对软链指回真实文件，其父目录仍是
    # `$out/lib/windinput`，`data/` 定位不受影响（见文件头）。
    mkdir -p $out/bin
    ln -s ../lib/windinput/wind_input $out/bin/wind_input
    runHook postInstall
  '';

  meta = {
    description = "清风输入法核心服务（Linux / Fcitx5 宿主）";
    homepage = "https://windinput.com";
    license = lib.licenses.gpl3Plus;
    mainProgram = "wind_input";
    platforms = lib.platforms.linux;
  };
}
