{
  description = "清风输入法 (WindInput) — Linux Fcitx5 打包";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    # 邻仓：设置程序与它依赖的 GUI 基础库。两者都不是 flake，按源码树取。
    wind-setting = {
      url = "github:huanfeng/wind-setting";
      flake = false;
    };
    wind-ui-rust = {
      url = "github:huanfeng/wind-ui-rust";
      flake = false;
    };
  };

  outputs =
    { self, nixpkgs, ... }:
    let
      systems = [
        "x86_64-linux"
        "aarch64-linux"
      ];
      version = nixpkgs.lib.removeSuffix "\n" (builtins.readFile ./docs/VERSION);
    in
    {
      packages = nixpkgs.lib.genAttrs systems (
        system:
        let
          pkgs = import nixpkgs { inherit system; };
          windinput-data = pkgs.callPackage ./nix/data.nix { inherit version; };
          windinput-service = pkgs.callPackage ./nix/service.nix {
            inherit version;
            # 服务启动日志里的 `git:<rev>`；沙箱里没有 .git，只能由这里给（unknown 表示
            # 从非 flake 路径构建，例如 `nix build path:.#…`）。
            rev = self.shortRev or self.dirtyShortRev or "unknown";
            # 版本戳 = 源码源的 `lastModified`（git 源取提交时间，`path:` 源取源码树 mtime），
            # 显式传给 service.nix —— stdenv 的兜底只是常量 315532800（1980-01-01），
            # 不传就会让二进制里的 build 恒为 1980。
            sourceDateEpoch = self.lastModified or 315532800;
            data = windinput-data;
          };
          wind-setting = pkgs.callPackage ./nix/setting.nix {
            inherit version;
            windInputSrc = self;
            windSettingSrc = self.inputs.wind-setting;
            windUiRustSrc = self.inputs.wind-ui-rust;
          };
          windinput-fcitx5 = pkgs.callPackage ./nix/addon.nix {
            inherit version;
            service = windinput-service;
            setting = wind-setting;
          };
          windinput = pkgs.callPackage ./nix/package.nix {
            inherit
              version
              windinput-service
              windinput-fcitx5
              wind-setting
              ;
          };
        in
        {
          inherit
            windinput
            windinput-data
            windinput-service
            windinput-fcitx5
            wind-setting
            ;
          default = windinput;
        }
      );

      devShells = nixpkgs.lib.genAttrs systems (
        system: {
          default = (import nixpkgs { inherit system; }).callPackage ./nix/devshell.nix { };
        }
      );
    };
}
