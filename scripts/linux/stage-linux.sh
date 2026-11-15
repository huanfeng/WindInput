#!/usr/bin/env bash
# 构建服务 + Fcitx5 addon + 设置程序，并组装成「安装树」（$STAGE_DIR，即包内 / 之下的文件布局）。
# deb 与 rpm 共用这一棵树，各自只补包元数据（control / spec）：
#   - build-deb.sh：本脚本之后加 DEBIAN/ 并 dpkg-deb；
#   - build-rpm.sh：拿本脚本产出（或 CI 里 deb 构建 job 上传的同一棵树）交给 rpmbuild。
# 必须在最老的目标基线（Ubuntu 22.04，glibc 2.35 / fcitx5 5.0.x）上跑，不要在别的发行版上直接跑
# （会引入更高版本的 glibc 符号，22.04 上装不起来）。
# 调用处：scripts/linux/package-deb.sh（本机 docker 容器里）；.github/workflows/linux-build.yml（原生 runner）。
#
# 环境变量（括号内为默认值）：
#   APP_VERSION    必填，产品版本（docs/VERSION 口径），注入设置程序与 addon
#   SRC_DIR        WindInput 仓库根（脚本所在仓库）
#   DATA_DIR       随包分发的词库/方案目录（<SRC_DIR>/build_dev/data）
#   SETTING_DIR    设置程序仓库（<SRC_DIR>/../wind-setting）；它经 path 依赖引用 ../WindInput 与 ../wind-ui-rust
#   WORK_DIR       构建缓存与暂存（/tmp/windinput-deb-work）
#   STAGE_DIR      安装树输出目录（<WORK_DIR>/stage/pkg；每次先清空）
#   SERVICE_TARGET / SETTING_TARGET   两个 cargo target 目录（<WORK_DIR>/target、<WORK_DIR>/target-setting）。
#                  必须是两份：与服务同名的 path 依赖（wind-ipc 等）在设置程序里从 SETTING_DIR 的兄弟
#                  布局解析，和服务从 SRC_DIR 解析的是两份源码路径，混用一个 target 只会互相作废缓存。
set -euxo pipefail

export PATH=/opt/cargo/bin:$PATH
: "${APP_VERSION:?}"
SRC_DIR="${SRC_DIR:-$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)}"
DATA_DIR="${DATA_DIR:-$SRC_DIR/build_dev/data}"
SETTING_DIR="${SETTING_DIR:-$SRC_DIR/../wind-setting}"
WORK_DIR="${WORK_DIR:-/tmp/windinput-deb-work}"
SERVICE_TARGET="${SERVICE_TARGET:-$WORK_DIR/target}"
SETTING_TARGET="${SETTING_TARGET:-$WORK_DIR/target-setting}"
STAGE_DIR="${STAGE_DIR:-$WORK_DIR/stage/pkg}"
mkdir -p "$WORK_DIR"

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
# addon 的版本（fcitx5 输入法列表里的 `Version=`）与产品版本同源。不传就落回
# project(wind_linux VERSION 0.1.0)，与 Nix 包（wind_linux/nix/addon.nix 传同一个值）显示的版本不一致。
cmake -S "$SRC_DIR/wind_linux" -B "$WORK_DIR/cmake" -G Ninja -DCMAKE_BUILD_TYPE=Release \
    -DCMAKE_INSTALL_PREFIX=/usr -DWIND_ADDON_VERSION="$APP_VERSION"
cmake --build "$WORK_DIR/cmake"
S="$STAGE_DIR"
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

