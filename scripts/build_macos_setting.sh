#!/usr/bin/env bash
# build_macos_setting.sh — 在 macOS 上构建 wind_setting.app (Wails+Vue 设置界面)。
#
# 处理本工程在新版工具链下的两个坑:
#   1. pnpm 11 对未批准的构建脚本 (vue-demi) 退出码非 0 —— 用 `|| true` 容忍
#   2. vue-tsc 严格类型检查失败 (TS6/Vite8) —— 直接用 vite build 跳过 tsc 门禁
#
# 并把程序数据 (data/: schemas/themes/词库) 拷进 .app, 因为设置界面按 exeDir/data
# 扫描内置方案与主题, 而 macOS .app 的可执行目录 (Contents/MacOS) 旁边没有 data。
#
# 前置: 已装 wails CLI (go install github.com/wailsapp/wails/v2/cmd/wails) + pnpm;
#       已跑过 scripts/build_macos.sh 生成 build/data (内置方案/主题/词库)。
#
# 输出: wind_setting/build/bin/wind_setting.app
set -euo pipefail

SCRIPT_DIR=$(cd "$(dirname "$0")" && pwd)
REPO_DIR=$(cd "$SCRIPT_DIR/.." && pwd)
SETTING_DIR="$REPO_DIR/wind_setting"
APP="$SETTING_DIR/build/bin/wind_setting.app"

bold() { printf "\033[1m%s\033[0m\n" "$*"; }
err()  { printf "\033[31m[错误] %s\033[0m\n" "$*" >&2; }

export PATH="$PATH:$(go env GOPATH)/bin"
command -v wails >/dev/null || { err "wails CLI 未安装: go install github.com/wailsapp/wails/v2/cmd/wails@v2.12.0"; exit 1; }
command -v pnpm  >/dev/null || { err "pnpm 未安装"; exit 1; }

cd "$SETTING_DIR"

bold "==> [1/5] 生成 Wails JS 绑定 (frontend/wailsjs)"
wails generate module

bold "==> [2/5] 安装前端依赖 (容忍 pnpm 11 的 ignored-builds 退出码)"
( cd frontend && pnpm install || true )

bold "==> [3/5] 构建前端 (vite, 跳过 vue-tsc 严格门禁)"
( cd frontend && ./node_modules/.bin/vite build )

bold "==> [4/5] 编译 + 打包 (wails build -s 跳过前端步骤, 自签名)"
wails build -s -platform darwin/arm64

[[ -d "$APP" ]] || { err "未生成 $APP"; exit 1; }

bold "==> [5/5] 把程序数据拷入 .app (设置界面按 exeDir/data 扫描方案/主题)"
if [[ -d "$REPO_DIR/build/data" ]]; then
    rm -rf "$APP/Contents/MacOS/data"
    cp -R "$REPO_DIR/build/data" "$APP/Contents/MacOS/data"
    printf "  data: %s 文件\n" "$(find "$APP/Contents/MacOS/data" -type f | wc -l | tr -d ' ')"
else
    err "未找到 $REPO_DIR/build/data, 先跑 scripts/build_macos.sh data"
    exit 1
fi

bold "==> Done"
printf "  Bundle: %s\n" "$APP"
printf "  安装: 复制到 /Applications/ 或由 IME 指示器菜单 '设置…' 按 bundleID 启动\n"
