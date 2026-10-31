# 汇总包：服务（含数据）+ addon + 设置程序 + 桌面集成 + 许可证。
#
# 用 symlinkJoin 是安全的：服务二进制在 windinput-service 里是真实文件，且 data/ 就在它旁边；
# `current_exe()` 经 /proc/self/exe 解析到那个真实 store 路径，不会落到本包的软链上。
# addon 编译期钉的也是 windinput-service 的路径，与这里怎么拼无关。
#
# 桌面入口 / MIME 定义沿用 scripts/linux/pkg/（deb 用的是同一份），只把 Exec 里的
# /usr/lib/windinput/wind_setting 换成 store 路径。
# 许可证（LICENSE、NOTICE.md）装进 share/licenses/：`scripts/pack-installer.sh` 的
# `licenses/` 装的是同一份，分发物应当自带到手用户手里。
{
  lib,
  symlinkJoin,
  version,
  windinput-service,
  windinput-fcitx5,
  wind-setting,
}:
symlinkJoin {
  name = "windinput-${version}";
  paths = [
    windinput-service
    windinput-fcitx5
    wind-setting
  ];

  postBuild = ''
    mkdir -p $out/share/applications $out/share/mime/packages $out/share/licenses/windinput
    for f in windinput-setting windinput-import; do
      install -m644 ${../scripts/linux/pkg}/$f.desktop $out/share/applications/$f.desktop
      substituteInPlace $out/share/applications/$f.desktop \
        --replace-fail /usr/lib/windinput/wind_setting ${wind-setting}/lib/windinput/wind_setting
    done
    install -m644 ${../scripts/linux/pkg}/windinput-mime.xml $out/share/mime/packages/windinput.xml
    # 本项目自身许可证（GPL-3.0-or-later）与上游资源声明；rime-emoji 的 LGPL-3.0 正文
    # 随数据分发（data/emoji/），NOTICE.md 只做声明。
    install -m644 ${../LICENSE} $out/share/licenses/windinput/LICENSE
    install -m644 ${../NOTICE.md} $out/share/licenses/windinput/NOTICE.md
  '';

  meta = {
    description = "清风输入法（Linux / Fcitx5）";
    homepage = "https://windinput.com";
    license = lib.licenses.gpl3Plus;
    # symlinkJoin 里只有 windinput-service 出 bin/（`$out/bin/wind_input`），
    # 不写这行 `nix run .#windinput` 会去找 bin/<本包名> 而失败。
    mainProgram = "wind_input";
    platforms = lib.platforms.linux;
  };
}
