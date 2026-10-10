#!/usr/bin/env bash
# 在 Ubuntu 22.04（glibc 2.35 / fcitx5 5.0.x）环境里构建并组装 .deb：服务 + Fcitx5 addon + 设置程序。
# 两处调用，都不要在别的发行版上直接跑（会引入更高版本的 glibc 符号，22.04 上装不起来）：
#   - scripts/linux/package-deb.sh：本机 docker 容器里（路径经下面的环境变量传入）；
#   - .github/workflows/linux-build.yml：ubuntu-22.04 / ubuntu-22.04-arm 原生 runner 上。
# 架构取 `dpkg --print-architecture`（amd64 / arm64），产物 <OUT_DIR>/windinput_<DEB_VERSION>_<arch>.deb。
#
# 环境变量（括号内为默认值）：
#   DEB_VERSION    必填，deb 的 Version 字段
#   APP_VERSION    必填，产品版本（docs/VERSION 口径），注入设置程序
#   SRC_DIR        WindInput 仓库根（脚本所在仓库）
#   DATA_DIR       随包分发的词库/方案目录（<SRC_DIR>/build_dev/data）
#   SETTING_DIR    设置程序仓库（<SRC_DIR>/../wind-setting）；它经 path 依赖引用 ../WindInput 与 ../wind-ui-rust
#   WORK_DIR       构建缓存与暂存（/tmp/windinput-deb-work）
#   OUT_DIR        产物目录（<WORK_DIR>/out）
#   SERVICE_TARGET / SETTING_TARGET   两个 cargo target 目录（<WORK_DIR>/target、<WORK_DIR>/target-setting）。
#                  必须是两份：与服务同名的 path 依赖（wind-ipc 等）在设置程序里从 SETTING_DIR 的兄弟
#                  布局解析，和服务从 SRC_DIR 解析的是两份源码路径，混用一个 target 只会互相作废缓存。
set -euxo pipefail

export PATH=/opt/cargo/bin:$PATH
: "${DEB_VERSION:?}" "${APP_VERSION:?}"
SRC_DIR="${SRC_DIR:-$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)}"
DATA_DIR="${DATA_DIR:-$SRC_DIR/build_dev/data}"
SETTING_DIR="${SETTING_DIR:-$SRC_DIR/../wind-setting}"
WORK_DIR="${WORK_DIR:-/tmp/windinput-deb-work}"
OUT_DIR="${OUT_DIR:-$WORK_DIR/out}"
SERVICE_TARGET="${SERVICE_TARGET:-$WORK_DIR/target}"
SETTING_TARGET="${SETTING_TARGET:-$WORK_DIR/target-setting}"
ARCH="$(dpkg --print-architecture)"
mkdir -p "$WORK_DIR" "$OUT_DIR"

# 不加 --locked，与 Windows / macOS 的发布构建一致：设置程序经 path 依赖引用 wind-ui-rust 等兄弟仓，
# 它们升版本后锁文件常常没跟上，--locked 会让 Linux 一端单独在发版时失败。
# 等这些依赖改走 crates.io、锁文件稳定下来，再三端统一加回 --locked。

# ── 服务 ──
(cd "$SRC_DIR/wind_input" && CARGO_TARGET_DIR="$SERVICE_TARGET" \
    cargo build --release -p wind_service --features linux-host)

# ── 设置程序 ──
# 版本号与服务同源（docs/VERSION），不走 git——worktree 的 .git 指向宿主路径，容器里读不到。
(cd "$SETTING_DIR" && WIND_APP_VERSION="$APP_VERSION" CARGO_TARGET_DIR="$SETTING_TARGET" \
    cargo build --release)

# ── addon ──
cmake -S "$SRC_DIR/wind_linux" -B "$WORK_DIR/cmake" -G Ninja -DCMAKE_BUILD_TYPE=Release -DCMAKE_INSTALL_PREFIX=/usr
cmake --build "$WORK_DIR/cmake"
S="$WORK_DIR/stage/pkg"
rm -rf "$S"
DESTDIR="$S" cmake --install "$WORK_DIR/cmake"

# ── 组装 ──
install -Dm755 $SERVICE_TARGET/release/wind_input "$S/usr/lib/windinput/wind_input"
mkdir -p "$S/usr/lib/windinput/data"
cp -a "$DATA_DIR/." "$S/usr/lib/windinput/data/"
# 应用兼容规则是平台专属策略：Windows 版 compat.toml 里全是 Windows 宿主的修正，不能随 Linux 包带。
# 换成「字段说明 + 零条内置规则」的 Linux 版（生成脚本自检不得残留规则表）。
bash "$SRC_DIR/scripts/lib/gen-compat.sh" linux "$DATA_DIR/compat.toml" "$S/usr/lib/windinput/data/compat.toml"
# addon / 输入法描述：cmake 只装了库，描述文件（构建目录里 configure_file 出来的）补进去。
install -Dm644 $WORK_DIR/cmake/share/fcitx5/addon/windinput.conf "$S/usr/share/fcitx5/addon/windinput.conf"
install -Dm644 $WORK_DIR/cmake/share/fcitx5/inputmethod/windinput.conf "$S/usr/share/fcitx5/inputmethod/windinput.conf"
install -Dm755 $SRC_DIR/scripts/linux/pkg/windinput-setup "$S/usr/bin/windinput-setup"
install -Dm755 $SETTING_TARGET/release/wind_setting "$S/usr/lib/windinput/wind_setting"
install -Dm644 $SRC_DIR/scripts/linux/pkg/windinput-setting.desktop "$S/usr/share/applications/windinput-setting.desktop"
install -Dm644 $SRC_DIR/scripts/linux/pkg/windinput-import.desktop "$S/usr/share/applications/windinput-import.desktop"
install -Dm644 $SRC_DIR/scripts/linux/pkg/windinput-mime.xml "$S/usr/share/mime/packages/windinput.xml"
# 图标（windinput / windinput-zh / windinput-en，各尺寸）由上面的 cmake --install 装好。
install -Dm644 $SRC_DIR/wind_linux/README.md "$S/usr/share/doc/windinput/README.md"

# 权限归一：`cp -a` 把宿主 umask（002 → 0775/0664）原样带进包，装到系统里就是 root 组可写。
# 目录 755；带执行位的文件 755、其余 644（符号链接不动）。
find "$S" -type d -exec chmod 755 {} +
find "$S" -type f -perm /111 -exec chmod 755 {} +
find "$S" -type f ! -perm /111 -exec chmod 644 {} +

# 链接期依赖交给 dpkg-shlibdeps 按真实 ELF 符号算（libc6 / libstdc++6 / libgcc-s1 / libfcitx5* / libxcb*），
# 而不是手写——以后升级编译器或第三方库时手写表很容易漏。算不到的才手写：fcitx5 本体与最低版本
# （addon 按 5.0.14 的头文件编，是发布基线）、运行时 dlopen 的 libfontconfig1（ELF 里没有 NEEDED）、
# 字体 / python3 / procps 这类非库依赖。
SHL="$WORK_DIR/shlibs"
rm -rf "$SHL"; mkdir -p "$SHL/debian"
printf 'Source: windinput\n\nPackage: windinput\nArchitecture: any\n' >"$SHL/debian/control"
SHLIBS_DEPENDS="$(cd "$SHL" && dpkg-shlibdeps -O \
    -e"$S/usr/lib/windinput/wind_input" -e"$S/usr/lib/windinput/wind_setting" \
    -e"$(find "$S/usr/lib" -name libwindinput.so -path '*fcitx5*')" | sed -n 's/^shlibs:Depends=//p')"
[[ -n "$SHLIBS_DEPENDS" ]] || { echo "dpkg-shlibdeps 没有算出依赖" >&2; exit 1; }
EXTRA_DEPENDS="fcitx5 (>= 5.0.14), libfontconfig1, fonts-noto-cjk | fonts-wqy-microhei | fonts-wqy-zenhei, python3, procps"

mkdir -p "$S/DEBIAN"
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
Recommends: fcitx5-frontend-gtk3, fcitx5-frontend-qt5 | fcitx5-frontend-qt6, im-config, xclip | wl-clipboard, xdg-utils, xdg-desktop-portal | zenity, libxkbcommon0, fonts-noto-color-emoji
Homepage: https://windinput.com
Description: 清风输入法 (WindInput) —— Fcitx5 输入法引擎
 清风输入法的 Linux 版（测试版）：Rust 服务负责输入逻辑与候选窗渲染，
 Fcitx5 addon 负责与应用对接，另带图形设置程序（应用菜单「清风输入法设置」，
 或在输入时按 Ctrl+Shift+]）。安装后运行 windinput-setup 配置当前用户。
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
    echo "清风输入法已安装。请对每个使用者运行一次： windinput-setup   然后注销并重新登录（升级则重启 fcitx5：fcitx5 -rd）。"
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
