#!/usr/bin/env bash
# 在 Ubuntu 22.04（glibc 2.35 / fcitx5 5.0.x）环境里构建并组装 .deb：服务 + Fcitx5 addon + 设置程序。
# 构建与安装树组装在 stage-linux.sh（与 rpm 共用），本脚本只加 deb 元数据并打包。
# 两处调用，都不要在别的发行版上直接跑（会引入更高版本的 glibc 符号，22.04 上装不起来）：
#   - scripts/linux/package-deb.sh：本机 docker 容器里（路径经下面的环境变量传入）；
#   - .github/workflows/linux-build.yml：ubuntu-22.04 / ubuntu-22.04-arm 原生 runner 上。
# 架构取 `dpkg --print-architecture`（amd64 / arm64），产物 <OUT_DIR>/windinput_<DEB_VERSION>_<arch>.deb。
#
# 环境变量同 stage-linux.sh，另有：
#   DEB_VERSION    必填，deb 的 Version 字段
#   OUT_DIR        产物目录（<WORK_DIR>/out）
set -euxo pipefail

export PATH=/opt/cargo/bin:$PATH
: "${DEB_VERSION:?}" "${APP_VERSION:?}"
SRC_DIR="${SRC_DIR:-$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)}"
WORK_DIR="${WORK_DIR:-/tmp/windinput-deb-work}"
OUT_DIR="${OUT_DIR:-$WORK_DIR/out}"
ARCH="$(dpkg --print-architecture)"
mkdir -p "$WORK_DIR" "$OUT_DIR"

# ── 构建 + 安装树 ──
export SRC_DIR WORK_DIR STAGE_DIR="$WORK_DIR/stage/pkg"
bash "$SRC_DIR/scripts/linux/stage-linux.sh"
S="$STAGE_DIR"

# 链接期依赖交给 dpkg-shlibdeps 按真实 ELF 符号算（libc6 / libstdc++6 / libgcc-s1 / libfcitx5* / libxcb*），
# 而不是手写——以后升级编译器或第三方库时手写表很容易漏。算不到的才手写：fcitx5 本体与最低版本
# （addon 按 5.0.14 的头文件编，是发布基线）、运行时 dlopen 的 libfontconfig1（ELF 里没有 NEEDED）、
# 字体 / procps 这类非库依赖。
SHL="$WORK_DIR/shlibs"
rm -rf "$SHL"; mkdir -p "$SHL/debian"
printf 'Source: windinput\n\nPackage: windinput\nArchitecture: any\n' >"$SHL/debian/control"
SHLIBS_DEPENDS="$(cd "$SHL" && dpkg-shlibdeps -O \
    -e"$S/usr/lib/windinput/wind_input" -e"$S/usr/lib/windinput/wind_setting" \
    -e"$(find "$S/usr/lib" -name libwindinput.so -path '*fcitx5*')" | sed -n 's/^shlibs:Depends=//p')"
[[ -n "$SHLIBS_DEPENDS" ]] || { echo "dpkg-shlibdeps 没有算出依赖" >&2; exit 1; }
EXTRA_DEPENDS="fcitx5 (>= 5.0.14), libfontconfig1, fonts-noto-cjk | fonts-wqy-microhei | fonts-wqy-zenhei, procps"

mkdir -p "$S/DEBIAN"
# 文件对话框：门户与 zenity 分开推荐。门户在、但没有文件选择后端时也弹不出来，rfd 那时不退
# zenity；设置端再兜底试 kdialog。写成「portal | zenity」的话装了门户就不装 zenity，兜底落空。
SIZE=$(du -sk --apparent-size "$S" | cut -f1)
cat >"$S/DEBIAN/control" <<CONTROL
Package: windinput
Version: $DEB_VERSION
Architecture: $ARCH
Maintainer: WindInput <noreply@windinput.com>
Section: utils
Priority: optional
Installed-Size: $SIZE
Depends: $SHLIBS_DEPENDS, $EXTRA_DEPENDS
Recommends: fcitx5-frontend-gtk3, fcitx5-frontend-qt5 | fcitx5-frontend-qt6, im-config, xclip | wl-clipboard, xdg-utils, xdg-desktop-portal, zenity | kdialog, libxkbcommon0, fonts-noto-color-emoji
Homepage: https://windinput.com
Description: 清风输入法 (WindInput) —— Fcitx5 输入法引擎
 清风输入法的 Linux 版（测试版）：Rust 服务负责输入逻辑与候选窗渲染，
 Fcitx5 addon 负责与应用对接，另带图形设置程序（应用菜单「清风输入法设置」，
 或在输入时按 Ctrl+Shift+]）。安装后在 Fcitx5 配置的输入法列表里添加「清风输入法」即可使用。
CONTROL
# 结束服务只认包里那个路径：addon 用 posix_spawn 拉起时 argv 恰为这一个全路径、不带参数，
# 菜单「重启服务」自拉起的再多一个 `--restarted`；`pkill -x -f` 要求整条命令行与模式完全
# 匹配——同名的开发构建、`wind_input restart` 这类 CLI 调用都不会被误杀。
# 装的是所有用户共用的二进制，升级 / 卸载时每个用户的旧实例都该结束，故不限 -u。
# 服务没有 SIGTERM 处理，被结束时防抖窗口（1s）内未落盘的运行时状态会丢（docs/design/linux-port.md §7）。
cat >"$S/DEBIAN/postinst" <<'POSTINST'
#!/bin/sh
set -e
if [ "$1" = configure ]; then
    # 升级时旧服务还在跑旧二进制：结束它，addon 在下次连不上时会自动拉起新版。
    # （已加载进 fcitx5 的旧 addon 需重启 fcitx5 或重新登录才换新。）
    pkill -x -f '/usr/lib/windinput/wind_input( --restarted)?' 2>/dev/null || true
    echo "清风输入法已安装。在 Fcitx5 配置的输入法列表里添加「清风输入法」即可使用（首次安装需注销并重新登录；升级只需重启 fcitx5：fcitx5 -rd）。"
    # GNOME 的 Wayland 会话没有输入法窗口定位接口，候选窗要靠随包的 Shell 扩展才跟得准光标，
    # 而扩展装完默认不启用（系统包碰不到用户的桌面配置）——这里只能提示。仅本机有 gnome-shell 时才说。
    if command -v gnome-shell >/dev/null 2>&1; then
        echo ""
        echo "【GNOME 用户请注意】若使用 Wayland 会话（Ubuntu 默认），候选窗需要启用随包的 GNOME 扩展才会跟随光标："
        echo "  1. 注销并重新登录（GNOME 只在登录时扫描新装的扩展）；"
        echo "  2. 打开「清风输入法设置」，点窗口顶部黄色提示条里的「启用」；"
        echo "     或在终端执行（GNOME 45 及以上）：gnome-extensions enable windinput-caret@windinput"
        echo "     （GNOME 42–44：gnome-extensions enable windinput-caret-legacy@windinput）；"
        echo "  3. 再注销重登一次。"
        echo "  X11 会话（Ubuntu on Xorg）不需要这一步。"
    fi
fi
exit 0
POSTINST
chmod 755 "$S/DEBIAN/postinst"
# 卸载放在 postrm 而不是 prerm：文件删掉之后再结束，addon（仍在已运行的 fcitx5 里）想重新
# 拉起时程序已不在（它先 access(X_OK)），不会在 prerm 与删文件之间又起一个孤儿。
cat >"$S/DEBIAN/postrm" <<'POSTRM'
#!/bin/sh
set -e
if [ "$1" = remove ]; then
    pkill -x -f '/usr/lib/windinput/wind_input( --restarted)?' 2>/dev/null || true
fi
exit 0
POSTRM
chmod 755 "$S/DEBIAN/postrm"

dpkg-deb --root-owner-group --build "$S" "$OUT_DIR/windinput_${DEB_VERSION}_${ARCH}.deb"
