#!/usr/bin/env bash
# install_setting.sh — 把 wind_setting.app (Wails+Vue 设置界面) 装到 per-user
# ~/Applications/, 并刷新 LaunchServices, 让 IME 指示器菜单的 "设置…" 能按 bundleID
# (com.wails.wind_setting) 经 NSWorkspace 找到并启动它.
#
# wind_setting 是普通 GUI 应用 (不是输入法), 装在 ~/Applications 即可被 LS 索引,
# 不需要 sudo, 也不进 /Library/Input Methods.
#
# 用法:
#   scripts_mac/deploy/install_setting.sh                 # 装 wind_setting/build/bin 的产物
#   scripts_mac/deploy/install_setting.sh --build         # 先 build (scripts_mac/build/setting.sh) 再装
#   scripts_mac/deploy/install_setting.sh --from <dir>    # 从指定目录装 (内含 wind_setting.app)
#   scripts_mac/deploy/install_setting.sh --uninstall     # 卸载
set -euo pipefail

SCRIPT_DIR=$(cd "$(dirname "$0")" && pwd)
REPO_DIR=$(cd "$SCRIPT_DIR/../.." && pwd)

# 变体: release → wind_setting / 清风输入法设置 / com.wails.wind_setting;
#       debug   → wind_setting_debug / 清风输入法设置开发版 / com.wails.wind_setting_debug。
# EXE_NAME 恒为 wind_setting (Wails outputfilename, 两变体可执行同名), pkill 按它匹配。
EXE_NAME="wind_setting"
APP_NAME="wind_setting"                  # 源 .app 文件名 (setting.sh 产物名)
DISPLAY_NAME="清风输入法设置"             # 安装后的 .app 文件名: Finder 显示用。实测 CFBundleDisplayName
                                         # 对 .app 的 Finder 标签无效 (仍显示文件名), 只有改文件名才生效。
BUNDLE_ID="com.wails.wind_setting"
INSTALL_DIR="$HOME/Applications"
LSREGISTER="/System/Library/Frameworks/CoreServices.framework/Frameworks/LaunchServices.framework/Support/lsregister"

DO_BUILD=0
DO_UNINSTALL=0
SRC_DIR=""
while [[ $# -gt 0 ]]; do
    case "$1" in
        --build)     DO_BUILD=1 ;;
        --debug)     APP_NAME="wind_setting_debug"; DISPLAY_NAME="清风输入法设置开发版"; BUNDLE_ID="com.wails.wind_setting_debug" ;;
        --uninstall) DO_UNINSTALL=1 ;;
        --from)      shift; SRC_DIR="${1:-}"; [[ -n "$SRC_DIR" ]] || { echo "[错误] --from 缺目录参数" >&2; exit 1; } ;;
        *) echo "[错误] 未知参数: $1" >&2; exit 1 ;;
    esac
    shift
done

# 变体派生 (参数解析后, 不依赖顺序)。
DEFAULT_APP="$REPO_DIR/wind_setting/build/bin/$APP_NAME.app"
SRC_APP="${SRC_DIR:+$SRC_DIR/$APP_NAME.app}"
INSTALL_APP="$INSTALL_DIR/$DISPLAY_NAME.app"
LEGACY_APP="$INSTALL_DIR/$APP_NAME.app"  # 旧版按源名 .app 装过的残留 (清理用)

bold() { printf "\033[1m%s\033[0m\n" "$*"; }
info() { printf "  %s\n" "$*"; }
err()  { printf "\033[31m[错误] %s\033[0m\n" "$*" >&2; }

if [[ $EUID -eq 0 ]]; then
    err "请以普通用户运行 (装到 ~/Applications, LS 注册是 per-user). 不要 sudo."
    exit 1
fi

# -------- uninstall --------
if [[ $DO_UNINSTALL -eq 1 ]]; then
    bold "==> Uninstall $APP_NAME"
    info "kill $APP_NAME 进程"
    pkill -f "Contents/MacOS/$EXE_NAME" 2>/dev/null || true
    for app in "$INSTALL_APP" "$LEGACY_APP"; do
        if [[ -d "$app" ]]; then
            rm -rf "$app"
            info "removed $app"
        fi
    done
    # *绝不* 对已删路径跑 `lsregister -u` (见 install_app.sh 血泪教训: 会污染 LS DB,
    # 让系统设置 "添加输入法" picker 对所有用户报错). .app 删掉后残留索引下次扫描自然失效.
    info "(LS 残留索引留待系统下次扫描自然失效, 不跑 lsregister -u)"
    bold "==> Done"
    exit 0
fi

# -------- build (可选) --------
if [[ $DO_BUILD -eq 1 ]]; then
    "$REPO_DIR/scripts_mac/build/setting.sh"
fi

# -------- 解析源 .app --------
[[ -n "$SRC_APP" ]] || SRC_APP="$DEFAULT_APP"
[[ -d "$SRC_APP" ]] || { err "未找到 $SRC_APP, 先跑 scripts_mac/build/setting.sh 或加 --build"; exit 1; }

# -------- install --------
bold "==> Install $SRC_APP -> $INSTALL_APP"

# 1. 关掉旧实例 (避免 cp 被持锁)
if pgrep -f "Contents/MacOS/$EXE_NAME" >/dev/null 2>&1; then
    info "停止旧 $APP_NAME 进程"
    pkill -f "Contents/MacOS/$EXE_NAME" 2>/dev/null || true
    sleep 0.5
fi

# 2. 复制 (装为中文名 .app; 同时清掉旧 wind_setting.app 残留)
mkdir -p "$INSTALL_DIR"
rm -rf "$INSTALL_APP" "$LEGACY_APP"
cp -R "$SRC_APP" "$INSTALL_APP"
info "已复制 $INSTALL_APP"

# 3. 去隔离属性 + ad-hoc 重签.
#    rsync/cp 跨机过来的 adhoc 应用, 经 NSWorkspace 程序化启动时可能被 Gatekeeper /
#    amfi cdhash 缓存拦下; 原地 --force 重签刷新签名, 去 quarantine 防首启拦截.
xattr -dr com.apple.quarantine "$INSTALL_APP" 2>/dev/null || true
if command -v codesign >/dev/null; then
    codesign --force --deep -s - "$INSTALL_APP" 2>/dev/null \
        && info "ad-hoc 重签" || info "(codesign 重签跳过, 非致命)"
fi

# 4. 刷新 LaunchServices, 让 NSWorkspace 能按 bundleID 找到本 .app.
#    只用 `lsregister -f -R <现存路径>` (非破坏性重新登记), 绝不对已删路径用 -u/-kill.
if [[ -x "$LSREGISTER" ]]; then
    "$LSREGISTER" -f -R "$INSTALL_APP" 2>&1 | tail -1 | sed 's/^/    /'
    info "lsregister -f 完成"
else
    info "(lsregister 不在标准位置, 跳过)"
fi

# 5. 验证 LS 能按 bundleID 解析
bold "==> Verify"
if [[ -x "$LSREGISTER" ]]; then
    # 按 .app 路径数实际注册项 (grep bundleID 会把 identifier/CFBundleIdentifier 两行都算)
    N=$("$LSREGISTER" -dump 2>/dev/null | grep "path:" | grep -c "$DISPLAY_NAME.app" || true)
    info "LS DB 内 $DISPLAY_NAME.app 注册路径数: ${N:-0} (期望 >=1; 含 deploy staging 副本属正常)"
fi
if open -gb "$BUNDLE_ID" 2>/dev/null; then
    info "✓ open -gb $BUNDLE_ID 成功 (LS 可定位)"
    pkill -f "Contents/MacOS/$EXE_NAME" 2>/dev/null || true
else
    info "✗ open -gb 失败 (GUI 会话外属正常; 登录会话里 IME 菜单 '设置' 应可启动)"
fi

bold "==> Done"
info "IME 指示器菜单点 '设置…' 即可打开 (NSWorkspace 按 $BUNDLE_ID 启动)"
info "卸载: scripts_mac/deploy/install_setting.sh --uninstall"
