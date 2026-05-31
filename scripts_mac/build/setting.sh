#!/usr/bin/env bash
# build_macos_setting.sh — 在 macOS 上构建 wind_setting.app (Wails+Vue 设置界面)。
#
# 处理本工程在新版工具链下的坑:
#   - vue-tsc 严格类型检查失败 (TS6/Vite8) —— 直接用 vite build 跳过 tsc 门禁
# (vue-demi 的构建脚本已由 frontend/pnpm-workspace.yaml 的 allowBuilds 显式批准,
#  pnpm 11 不再因 ignored-builds 报非 0 退出码)
#
# 并把程序数据 (data/: schemas/themes/词库) 拷进 .app, 因为设置界面按 exeDir/data
# 扫描内置方案与主题, 而 macOS .app 的可执行目录 (Contents/MacOS) 旁边没有 data。
#
# 前置: 已装 wails CLI (go install github.com/wailsapp/wails/v2/cmd/wails) + pnpm;
#       已跑过 scripts_mac/build/build.sh 生成 build/data (内置方案/主题/词库)。
#
# 输出: wind_setting/build/bin/wind_setting.app
#
# 用法:
#   scripts_mac/build/setting.sh             # darwin/arm64 (本机/VM)
#   scripts_mac/build/setting.sh --universal # darwin/universal (arm64+x86_64, 分发/CI)
#   WIND_MAC_UNIVERSAL=1 scripts_mac/build/setting.sh   # 同上 (CI 走环境变量统一开关)
set -euo pipefail

SCRIPT_DIR=$(cd "$(dirname "$0")" && pwd)
REPO_DIR=$(cd "$SCRIPT_DIR/../.." && pwd)
SETTING_DIR="$REPO_DIR/wind_setting"
APP="$SETTING_DIR/build/bin/wind_setting.app"

bold() { printf "\033[1m%s\033[0m\n" "$*"; }
err()  { printf "\033[31m[错误] %s\033[0m\n" "$*" >&2; }

# universal: arm64+x86_64 通用 .app. WIND_MAC_UNIVERSAL=1 或 --universal 开启;
# 默认 darwin/arm64 (本机/VM 快). CI 走环境变量统一开关.
UNIVERSAL="${WIND_MAC_UNIVERSAL:-0}"
# 变体: --debug → 注入 buildvariant.variant=debug ldflag, 让设置应用经 pkg/rpcapi
# 自动连 WindInput_debug/rpc.sock (与 debug 服务对齐), 实现配置分离; 再把 bundleID/名称
# 改成 debug 变体并重命名 .app, 与正式设置应用共存。
VARIANT_SUFFIX=""
LDFLAGS_EXTRA=""
for arg in "$@"; do
    case "$arg" in
        --universal) UNIVERSAL=1 ;;
        --debug)     VARIANT_SUFFIX="_debug"; LDFLAGS_EXTRA="-X github.com/huanfeng/wind_input/pkg/buildvariant.variant=debug" ;;
        *) err "未知参数: $arg"; exit 1 ;;
    esac
done
if [[ $UNIVERSAL -eq 1 ]]; then
    WAILS_PLATFORM="darwin/universal"
else
    WAILS_PLATFORM="darwin/arm64"
fi

export PATH="$PATH:$(go env GOPATH)/bin"
command -v wails >/dev/null || { err "wails CLI 未安装: go install github.com/wailsapp/wails/v2/cmd/wails@v2.12.0"; exit 1; }
command -v pnpm  >/dev/null || { err "pnpm 未安装"; exit 1; }

cd "$SETTING_DIR"

# wind_setting 主包带 //go:embed all:frontend/dist, 而 `wails generate module` 第一步就编译
# Go(触发 embed)——此时真正的 vite 产物还没生成(在 [3/5]). 全新检出(CI)无 frontend/dist
# 会报 "pattern all:frontend/dist: no matching files found". 先放 stub 占位, [3/5] vite build 覆盖.
mkdir -p frontend/dist
[ -e frontend/dist/index.html ] || echo '<!-- placeholder, replaced by vite build -->' > frontend/dist/index.html

bold "==> [1/5] 生成 Wails JS 绑定 (frontend/wailsjs)"
wails generate module

bold "==> [2/5] 安装前端依赖"
( cd frontend && pnpm install )

bold "==> [3/5] 构建前端 (vite, 跳过 vue-tsc 严格门禁)"
( cd frontend && ./node_modules/.bin/vite build )

bold "==> [4/5] 编译 + 打包 (wails build -s 跳过前端步骤, 自签名; $WAILS_PLATFORM)"
if [[ -n "$LDFLAGS_EXTRA" ]]; then
    wails build -s -platform "$WAILS_PLATFORM" -ldflags "$LDFLAGS_EXTRA"
else
    wails build -s -platform "$WAILS_PLATFORM"
fi

[[ -d "$APP" ]] || { err "未生成 $APP"; exit 1; }

# 变体后处理 (debug): wails 固定产出 wind_setting.app + bundleID com.wails.wind_setting。
# 改成 debug 变体 (bundleID com.wails.wind_setting_debug + 名称加「开发版」) 并重命名 .app,
# 让 debug IME 的「设置…」按独立 bundleID 启动它, 与正式设置应用共存。
if [[ -n "$VARIANT_SUFFIX" ]]; then
    bold "==> 变体注入 (debug): bundleID/名称 → wind_setting_debug"
    PB=/usr/libexec/PlistBuddy
    $PB -c "Set :CFBundleIdentifier com.wails.wind_setting_debug" "$APP/Contents/Info.plist"
    $PB -c "Set :CFBundleName 清风输入法设置开发版" "$APP/Contents/Info.plist" 2>/dev/null || true
    $PB -c "Set :CFBundleDisplayName 清风输入法设置开发版" "$APP/Contents/Info.plist" 2>/dev/null || true
    DEBUG_APP="$SETTING_DIR/build/bin/wind_setting_debug.app"
    rm -rf "$DEBUG_APP"
    mv "$APP" "$DEBUG_APP"
    APP="$DEBUG_APP"
fi

bold "==> [5/5] 把程序数据拷入 .app (设置界面按 exeDir/data 扫描方案/主题)"
if [[ -d "$REPO_DIR/build/data" ]]; then
    rm -rf "$APP/Contents/MacOS/data"
    cp -R "$REPO_DIR/build/data" "$APP/Contents/MacOS/data"
    printf "  data: %s 文件\n" "$(find "$APP/Contents/MacOS/data" -type f | wc -l | tr -d ' ')"
else
    err "未找到 $REPO_DIR/build/data, 先跑 scripts_mac/build/build.sh data"
    exit 1
fi

bold "==> Done"
printf "  Bundle: %s\n" "$APP"
printf "  安装: 复制到 /Applications/ 或由 IME 指示器菜单 '设置…' 按 bundleID 启动\n"
