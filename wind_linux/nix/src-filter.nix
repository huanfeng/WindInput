# 派生输入的构建产物过滤：把 cargo 的 `target/` 从源码副本里剔掉。
#
# `path:` 源下 flake 整棵树（含未跟踪文件）都会被复制进 store，`wind_input/target/` 因此会成为
# service / tools / 设置程序的派生输入：本机每跑一次 `cargo build` 就换一个新 store 路径，
# 级联重建 data → service → fcitx5 → 汇总包；旧产物还会随源码进沙箱。
# 只剔构建产物，源码改动照常进输入哈希（那是刻意的耦合，见 `data.nix` 顶部注释）。
#
# 判据按路径段匹配，因为三处调用方的相对根不同：service / tools 以 `wind_input/` 为根
# （`target`），设置程序以仓库根为根（`wind_input/target`）。
{ lib }:
let
  buildOutputDirs = [ "target" ];

  # `rel` 是相对调用方源码根的路径。
  isBuildOutput = rel: lib.any (d: lib.hasInfix "/${d}/" "/${rel}/") buildOutputDirs;
in
{
  inherit isBuildOutput;

  # 直接当 `src` 用的源码树：套一层只剔构建产物的 `cleanSourceWith`。
  withoutBuildOutput =
    src:
    lib.cleanSourceWith {
      inherit src;
      filter =
        path: _type:
        !(isBuildOutput (lib.removePrefix "${toString src}/" (toString path)));
    };
}
