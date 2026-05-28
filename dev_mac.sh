#!/usr/bin/env bash
# dev_mac.sh — macOS 开发助手 (放在仓库根), 对位 dev.ps1.
#
# 当前覆盖:
#   服务侧 (Go 后端):
#     1   构建 release (Go 服务 + 数据)
#     1d  构建 debug variant
#     2   仅构建 Go 服务 (跳过词库下载)
#     r   构建 + 启动服务 (前台, debug 日志)
#     rd  构建 debug + 启动 debug 服务
#     s   仅启动已构建的服务 (前台)
#     sd  启动已构建的 debug 服务 (前台)
#     stop  停止运行中的服务 (通过 pid 文件)
#     smoke 调起 wind_macos/ 的 swift run wind-smoke
#     clean 清 build/ 与 build_debug/
#
#   IME 侧 (.app 安装/调试, 需 sudo):
#     app        只构建 WindInput.app (不装)
#     install    构建 + sudo 装到 /Library/Input Methods/
#     reinstall  uninstall + install (清干净再装, 修幽灵条目)
#     uninstall  sudo 完整清理 (.app + HIToolbox plist + caches + 守护进程)
#     tis        显示当前 TIS 内 WindInput 相关条目 (调试用)
set -euo pipefail

REPO_DIR=$(cd "$(dirname "$0")" && pwd)
BUILD_SCRIPT="$REPO_DIR/scripts/build_macos.sh"
PID_FILE="$HOME/Library/Application Support/WindInput/wind_input.pid"

usage() {
    cat <<EOF
WindInput - Dev Menu (macOS)

  -- 服务 (Go 后端) --
  1         构建 release (Go 服务 + 数据)
  1d        构建 debug variant
  2         仅构建 Go 服务 (跳过词库)
  r         构建 + 启动服务 (前台 debug 日志)
  rd        构建 debug + 启动 debug 服务
  s         启动已构建的 release 服务 (前台)
  sd        启动已构建的 debug 服务 (前台)
  stop      停止运行中的服务
  smoke     swift run wind-smoke (wind_macos/ 协议验证)
  clean     清 build/ 与 build_debug/

  -- IME (.app 安装与调试, 需要 sudo 时会提示密码) --
  app       只构建 WindInput.app
  install   构建 + 装到 /Library/Input Methods/
  reinstall 卸载干净 + 重新安装 (修复幽灵条目)
  uninstall 完整卸载 (.app + HIToolbox + caches + 守护进程)
  tis       显示 TIS 内 WindInput 当前条目

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

# ---- IME (.app) ----
do_app()       { bash "$REPO_DIR/scripts/build_macos_app.sh"; }
do_install()   { sudo bash "$REPO_DIR/scripts/install_macos_app.sh" --build; }
do_uninstall() { sudo bash "$REPO_DIR/scripts/install_macos_app.sh" --uninstall; }
do_reinstall() {
    sudo bash "$REPO_DIR/scripts/install_macos_app.sh" --uninstall
    sudo bash "$REPO_DIR/scripts/install_macos_app.sh" --build
}
do_tis() {
    if [[ ! -f "$REPO_DIR/scripts/list_input_sources.swift" ]]; then
        echo "[错误] 未找到 scripts/list_input_sources.swift" >&2; exit 1
    fi
    echo "==> TIS 内 WindInput / 相关条目 (huanfeng / wind / qingg / imkit)"
    swift "$REPO_DIR/scripts/list_input_sources.swift" 2>/dev/null \
        | grep -iE "huanfeng|wind|qingg|aodaren|imkit" | sed 's/^/  /' \
        || echo "  (无)"
}

case "$CHOICE" in
    1)         do_build all ;;
    1d)        do_build all --debug ;;
    2)         do_build service ;;
    r)         do_build all; do_run "$REPO_DIR/build/wind_input" ;;
    rd)        do_build all --debug; do_run "$REPO_DIR/build_debug/wind_input_debug" ;;
    s)         do_run "$REPO_DIR/build/wind_input" ;;
    sd)        do_run "$REPO_DIR/build_debug/wind_input_debug" ;;
    stop)      do_stop ;;
    smoke)     do_smoke "${2:-10}" ;;
    clean)     do_build clean ;;
    app)       do_app ;;
    install)   do_install ;;
    reinstall) do_reinstall ;;
    uninstall) do_uninstall ;;
    tis)       do_tis ;;
    *)         echo "[错误] 未知选项: $CHOICE" >&2; usage; exit 1 ;;
esac
