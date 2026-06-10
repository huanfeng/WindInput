#!/usr/bin/env bash
# dev_mac.sh — macOS 开发助手 (仓库根), 对位 dev.ps1.
# 菜单见 usage(); 调试版统一加前缀 d (如 d1 / d2 / dr / di).
set -euo pipefail

REPO_DIR=$(cd "$(dirname "$0")" && pwd)
MAC="$REPO_DIR/scripts_mac"
PID_FILE="$HOME/Library/Application Support/WindInput/wind_input.pid"

usage() {
    cat <<'EOF'
WindInput - Dev Menu (macOS)     调试版加前缀 d (如 d1 / d2 / dr / di / du)

  -- 构建 --
  1        构建全部 (Go 服务 + 词库 + IME .app + 设置应用)
  2        仅构建 Go 服务 (跳过词库下载)
  app      仅构建 IME .app bundle
  setting  仅构建设置应用 wind_setting.app (Wails)
  pkg      打分发安装包 .pkg (余参透传 pkg.sh: --build 先构建 / --universal 双架构)
  clean    清 build/ 与 build_debug/

  -- 本机安装 / 卸载 --
  i         本机安装全部 (Go 服务 LaunchAgent + IME .app + 设置应用)
  m <模块…> 单模块 构建+安装 (模块: service / app / setting; 如 m setting / m service app)
  redeploy  IME .app 重签 + 重装 + TIS 验证 (需 SIGN_IDENTITY, macOS 26 主入口)
  u         本机卸载全部 (Go 服务 + IME .app + 设置应用)

  -- 远程 VM (余参透传给 scripts_mac/vm/deploy.sh) --
  deploy    host→VM 一键部署 (服务 + .app; 加 --setting 含设置应用)
  undeploy  host→VM 远程卸载 + 验证清除
            (附加 --service-only / --app-only / --setting / --setting-only
             / 目标 admin@ip 均透传)

  -- 运行 / 诊断 (macOS IME 专用) --
  r      前台运行 Go 服务 (debug 日志)
  stop   停止前台 Go 服务 (按 pid 文件)
  smoke  协议验证 (swift run wind-smoke [秒])
  tis    显示 TIS 内 WindInput 注册状态

用法: ./dev_mac.sh [菜单代码] [附加参数]
EOF
}

CHOICE="${1:-}"
if [[ -z "$CHOICE" ]]; then
    usage
    printf "请选择: "
    read -r CHOICE
fi
[[ -z "$CHOICE" ]] && { usage; exit 1; }

# 调试版前缀 d: 白名单显式列出, 避免误剥离 deploy 的首字母 d.
VARIANT=""
case "$CHOICE" in
    d1)   VARIANT="--debug"; CHOICE="1" ;;
    d2)   VARIANT="--debug"; CHOICE="2" ;;
    dapp) VARIANT="--debug"; CHOICE="app" ;;
    di)   VARIANT="--debug"; CHOICE="i" ;;
    du)   VARIANT="--debug"; CHOICE="u" ;;
    dr)   VARIANT="--debug"; CHOICE="r" ;;
esac

# ---- 构建 ----
# setting.sh 支持 --debug: 注入 buildvariant.variant=debug ldflag + 产出 wind_setting_debug.app。
# VARIANT 同样透传给三件套, 保证 debug 全链路（服务/IME/.app/设置）使用同一变体。
do_build_all()     { "$MAC/build/build.sh" all ${VARIANT:+$VARIANT}; "$MAC/build/app.sh" ${VARIANT:+$VARIANT}; "$MAC/build/setting.sh" ${VARIANT:+$VARIANT}; }
do_build_svc()     { "$MAC/build/build.sh" service ${VARIANT:+$VARIANT}; }
do_build_app()     { "$MAC/build/app.sh" ${VARIANT:+$VARIANT}; }
do_build_setting() { "$MAC/build/setting.sh" ${VARIANT:+$VARIANT}; }
do_clean()         { "$MAC/build/build.sh" clean; }
# 分发安装包 .pkg (面向终端用户). 余参透传 pkg.sh: --build 先构建三件套 / --universal 双架构.
# 始终打 release 产物, 与 VARIANT(debug) 无关, 故不透传 $VARIANT.
do_pkg()           { bash "$MAC/build/pkg.sh" "$@"; }

# ---- 安装 / 部署 ----
# install_service.sh / install_app.sh / install_setting.sh 均为 per-user (装到 ~/Library
# 或 ~/Applications), 都不要 sudo.
do_install() {
    "$MAC/deploy/install_service.sh" ${VARIANT:+$VARIANT}
    # 必须把 $VARIANT 透传给 install_app.sh: 否则 di (--debug) 会装成 release .app,
    # 造成前端(release)/后端(debug) 变体错配, socket 路径隔离导致连不上 → 打字无候选。
    bash "$MAC/deploy/install_app.sh" ${VARIANT:+$VARIANT}
    # 同样透传 $VARIANT: debug 变体需装 清风输入法设置开发版.app (bundleID com.wails.wind_setting_debug),
    # 否则 debug IME 菜单「设置…」找不到对应 app, 用户打开的是 release 设置（连 release socket）。
    bash "$MAC/deploy/install_setting.sh" ${VARIANT:+$VARIANT}
}
do_redeploy() { bash "$MAC/deploy/redeploy.sh"; }
do_deploy()   { bash "$MAC/vm/deploy.sh" "$@"; }

# 单模块 构建+安装 (对位 Windows dev.ps1 的 m[N]). 模块: service / app / setting.
do_module() {
    [[ $# -gt 0 ]] || { echo "[错误] m 需指定模块: service / app / setting (如 m setting)" >&2; exit 1; }
    local mod
    for mod in "$@"; do
        case "$mod" in
            service|svc)
                "$MAC/build/build.sh" service ${VARIANT:+$VARIANT}
                "$MAC/deploy/install_service.sh" ${VARIANT:+$VARIANT} ;;
            app)
                "$MAC/build/app.sh" ${VARIANT:+$VARIANT}
                bash "$MAC/deploy/install_app.sh" ${VARIANT:+$VARIANT} ;;
            setting|set)
                "$MAC/build/setting.sh" ${VARIANT:+$VARIANT}
                bash "$MAC/deploy/install_setting.sh" ${VARIANT:+$VARIANT} ;;
            *) echo "[错误] 未知模块: $mod (可选 service / app / setting)" >&2; exit 1 ;;
        esac
    done
}

# ---- 卸载 ----
do_uninstall() {
    # du (--debug) 须卸载 debug 变体; 不传 $VARIANT 会误卸 release、留下 debug 残留。
    "$MAC/deploy/install_service.sh" --uninstall ${VARIANT:+$VARIANT}
    bash "$MAC/deploy/install_app.sh" --uninstall ${VARIANT:+$VARIANT}
    bash "$MAC/deploy/install_setting.sh" --uninstall ${VARIANT:+$VARIANT}
}

# ---- 运行 / 诊断 ----
do_run() {
    local exe
    if [[ -n "$VARIANT" ]]; then exe="$REPO_DIR/build_debug/wind_input_debug"; else exe="$REPO_DIR/build/wind_input"; fi
    [[ -x "$exe" ]] || { echo "[错误] 未找到 $exe, 先构建 (1 / 2 / d1 / d2)" >&2; exit 1; }
    cd "$(dirname "$exe")"
    echo "==> 启动 $exe (Ctrl+C 退出)"
    WIND_INPUT_LOG_LEVEL=debug ./"$(basename "$exe")"
}
do_stop() {
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
do_smoke() {
    cd "$REPO_DIR/wind_macos"
    swift run wind-smoke "${1:-10}"
}
do_tis() {
    local swift_tool="$MAC/test/list_input_sources.swift"
    [[ -f "$swift_tool" ]] || { echo "[错误] 未找到 $swift_tool" >&2; exit 1; }
    echo "==> TIS 内 WindInput / 相关条目 (huanfeng / wind / qingg / imkit)"
    swift "$swift_tool" 2>/dev/null \
        | grep -iE "huanfeng|wind|qingg|aodaren|imkit" | sed 's/^/  /' \
        || echo "  (无)"
}

case "$CHOICE" in
    1)         do_build_all ;;
    2)         do_build_svc ;;
    app)       do_build_app ;;
    setting)   do_build_setting ;;
    pkg)       do_pkg "${@:2}" ;;
    clean)     do_clean ;;
    i)         do_install ;;
    m)         do_module "${@:2}" ;;
    redeploy)  do_redeploy ;;
    u)         do_uninstall ;;
    deploy)    do_deploy "${@:2}" ;;
    undeploy)  do_deploy --uninstall "${@:2}" ;;
    r)         do_run ;;
    stop)      do_stop ;;
    smoke)     do_smoke "${2:-10}" ;;
    tis)       do_tis ;;
    *)         echo "[错误] 未知选项: $CHOICE" >&2; usage; exit 1 ;;
esac
