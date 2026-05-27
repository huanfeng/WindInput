#!/usr/bin/env bash
# dev_mac.sh — macOS 开发助手 (放在仓库根), 对位 dev.ps1.
#
# 当前覆盖 (PR-A 阶段):
#   1   构建 release (Go 服务 + 数据)
#   1d  构建 debug variant
#   2   仅构建 Go 服务 (跳过词库下载)
#   r   构建 + 启动服务 (前台, debug 日志)
#   rd  构建 debug + 启动 debug 服务
#   s   仅启动已构建的服务 (前台)
#   stop  停止运行中的服务 (通过 pid 文件)
#   clean 清 build/ 与 build_debug/
#   smoke 调起 wind_macos/ 的 swift run wind-smoke
#
# 未覆盖 (尚未在 macOS 上实现):
#   - 安装/卸载流程 (PR-A.5 M6)
#   - 便携模式 (Win-only 概念)
#   - wind_tsf DLL / wind_setting (跨平台 PR 时再补)
set -euo pipefail

REPO_DIR=$(cd "$(dirname "$0")" && pwd)
BUILD_SCRIPT="$REPO_DIR/scripts/build_macos.sh"
PID_FILE="$HOME/Library/Application Support/WindInput/wind_input.pid"

usage() {
    cat <<EOF
WindInput - Dev Menu (macOS)

  1       构建 release (Go 服务 + 数据)
  1d      构建 debug variant
  2       仅构建 Go 服务 (跳过词库)
  r       构建 + 启动服务 (前台 debug 日志)
  rd      构建 debug + 启动 debug 服务
  s       启动已构建的 release 服务 (前台)
  sd      启动已构建的 debug 服务 (前台)
  stop    停止运行中的服务
  smoke   swift run wind-smoke (wind_macos/ 协议验证)
  clean   清 build/ 与 build_debug/

用法:
  ./dev_mac.sh [菜单代码]
EOF
}

CHOICE="${1:-}"
if [[ -z "$CHOICE" ]]; then
    usage
    printf "请选择: "
    read -r CHOICE
fi
[[ -z "$CHOICE" ]] && { usage; exit 1; }

do_build()   { "$BUILD_SCRIPT" "$@"; }
do_run()     {
    local exe="$1"
    [[ -x "$exe" ]] || { echo "[错误] 未找到 $exe, 先构建" >&2; exit 1; }
    cd "$(dirname "$exe")"
    echo "==> 启动 $exe (Ctrl+C 退出)"
    WIND_INPUT_LOG_LEVEL=debug ./"$(basename "$exe")"
}
do_stop()    {
    if [[ -f "$PID_FILE" ]]; then
        local pid
        pid=$(cat "$PID_FILE")
        if kill -0 "$pid" 2>/dev/null; then
            kill "$pid"
            echo "已发送 SIGTERM 到 pid=$pid"
        else
            echo "pid $pid 已不在运行, 清理 pid 文件"
            rm -f "$PID_FILE"
        fi
    else
        echo "无 pid 文件, 未发现运行中的服务"
    fi
}
do_smoke()   {
    cd "$REPO_DIR/wind_macos"
    swift run wind-smoke "${@:-10}"
}

case "$CHOICE" in
    1)     do_build all ;;
    1d)    do_build all --debug ;;
    2)     do_build service ;;
    r)     do_build all; do_run "$REPO_DIR/build/wind_input" ;;
    rd)    do_build all --debug; do_run "$REPO_DIR/build_debug/wind_input_debug" ;;
    s)     do_run "$REPO_DIR/build/wind_input" ;;
    sd)    do_run "$REPO_DIR/build_debug/wind_input_debug" ;;
    stop)  do_stop ;;
    smoke) do_smoke "${2:-10}" ;;
    clean) do_build clean ;;
    *)     echo "[错误] 未知选项: $CHOICE" >&2; usage; exit 1 ;;
esac
