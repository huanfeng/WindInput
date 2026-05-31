#!/usr/bin/env bash
# install_macos_service.sh — 把 Go 服务 (wind_input + data/) 装到 per-user 目录,
# 并以 LaunchAgent 形式注册为开机自启常驻进程.
#
# 服务定位词库用 exeDir/data (见 pkg/config/paths.go GetDataDir), 所以二进制和 data/
# 必须同目录. 用户数据 (config.yaml / user_data.db / socket) 走 GetConfigDir
# (~/Library/Application Support/WindInput), 与本脚本安装的 service/ 子目录互不干扰.
#
# 以普通用户运行 (LaunchAgent 是 per-user 的 gui domain, 不要 sudo).
#
# 用法:
#   scripts_mac/deploy/install_service.sh                 # 装 repo build/ 的 release 产物
#   scripts_mac/deploy/install_service.sh --debug         # 装 build_debug/ 的 debug 产物
#   scripts_mac/deploy/install_service.sh --from <dir>    # 从指定目录装 (内含 wind_input + data/)
#   scripts_mac/deploy/install_service.sh --uninstall     # 卸载服务 (保留用户数据)
set -euo pipefail

SCRIPT_DIR=$(cd "$(dirname "$0")" && pwd)
REPO_DIR=$(cd "$SCRIPT_DIR/../.." && pwd)

LOG_DIR="$HOME/Library/Logs"
GUI_DOMAIN="gui/$(id -u)"

DEBUG_VARIANT=0
DO_UNINSTALL=0
SRC_DIR=""
EXE_NAME="wind_input"
while [[ $# -gt 0 ]]; do
    case "$1" in
        --debug)     DEBUG_VARIANT=1; EXE_NAME="wind_input_debug" ;;
        --uninstall) DO_UNINSTALL=1 ;;
        --from)      shift; SRC_DIR="${1:-}"; [[ -n "$SRC_DIR" ]] || { echo "[错误] --from 缺目录参数" >&2; exit 1; } ;;
        *) echo "[错误] 未知参数: $1" >&2; exit 1 ;;
    esac
    shift
done

# 变体派生 (参数解析后): debug 用独立 LaunchAgent label + 运行时目录 (WindInput_debug,
# 与 Go buildvariant.Suffix() 及 .app BridgeEndpoints.runtimeDir 对齐) + 中文名加「开发版」
# + 独立 launchd 日志, 让 debug/release 两套服务共存, 各连各自的 .app socket。
# 安装后的可执行名装为中文名: macOS 后台列表 (BTM) 对无 Developer ID 签名的 legacy agent
# 直接显示可执行文件名 (AssociatedBundleIdentifiers 被忽略)。二进制改名不影响功能。
if [[ $DEBUG_VARIANT -eq 1 ]]; then
    LABEL="to.feng.windinput.service.debug"
    APP_SUPPORT="$HOME/Library/Application Support/WindInput_debug"
    SVC_EXE_NAME="清风输入法服务开发版"
    ASSOC_BUNDLE="to.feng.inputmethod.WindInputDebug"
    LOG_TAG="windinput_debug"
else
    LABEL="to.feng.windinput.service"
    APP_SUPPORT="$HOME/Library/Application Support/WindInput"
    SVC_EXE_NAME="清风输入法服务"
    ASSOC_BUNDLE="to.feng.inputmethod.WindInput"
    LOG_TAG="windinput"
fi
INSTALL_ROOT="$APP_SUPPORT/service"
INSTALL_EXE="$INSTALL_ROOT/$SVC_EXE_NAME"
PLIST="$HOME/Library/LaunchAgents/$LABEL.plist"
OUT_LOG="$LOG_DIR/$LOG_TAG.out.log"
ERR_LOG="$LOG_DIR/$LOG_TAG.err.log"
PUSH_SOCK="$APP_SUPPORT/bridge_push.sock"

bold() { printf "\033[1m%s\033[0m\n" "$*"; }
info() { printf "  %s\n" "$*"; }
err()  { printf "\033[31m[错误] %s\033[0m\n" "$*" >&2; }

if [[ $EUID -eq 0 ]]; then
    err "请以普通用户运行 (LaunchAgent 是 per-user gui domain). 不要 sudo."
    exit 1
fi

# -------- uninstall --------
if [[ $DO_UNINSTALL -eq 1 ]]; then
    bold "==> Uninstall Go service ($LABEL)"
    # bootout 当前 domain 里的 service (现代 launchctl; legacy unload 已废弃).
    if launchctl print "$GUI_DOMAIN/$LABEL" >/dev/null 2>&1; then
        launchctl bootout "$GUI_DOMAIN/$LABEL" 2>/dev/null || true
        info "bootout $GUI_DOMAIN/$LABEL"
    else
        info "(service 未加载)"
    fi
    [[ -f "$PLIST" ]] && { rm -f "$PLIST"; info "removed $PLIST"; } || info "(no $PLIST)"
    # 只删 service/ 子目录 (二进制+预制词库), 保留用户数据 (../config.yaml, user_data.db).
    [[ -d "$INSTALL_ROOT" ]] && { rm -rf "$INSTALL_ROOT"; info "removed $INSTALL_ROOT"; } || info "(no $INSTALL_ROOT)"
    bold "==> Done (用户数据保留在 ~/Library/Application Support/WindInput/)"
    exit 0
fi

# -------- 解析源目录 --------
if [[ -z "$SRC_DIR" ]]; then
    if [[ $DEBUG_VARIANT -eq 1 ]]; then
        SRC_DIR="$REPO_DIR/build_debug"
    else
        SRC_DIR="$REPO_DIR/build"
    fi
fi
SRC_EXE="$SRC_DIR/$EXE_NAME"
SRC_DATA="$SRC_DIR/data"

[[ -f "$SRC_EXE" ]]  || { err "未找到二进制 $SRC_EXE, 先跑 scripts_mac/build/build.sh"; exit 1; }
[[ -d "$SRC_DATA" ]] || { err "未找到词库目录 $SRC_DATA, 先跑 scripts_mac/build/build.sh data"; exit 1; }

# -------- install --------
bold "==> Install Go service -> $INSTALL_ROOT"

# 1. 停旧服务 (若已注册过, 含 VM 上手动建的旧 plist; 同 Label 直接接管).
if launchctl print "$GUI_DOMAIN/$LABEL" >/dev/null 2>&1; then
    info "停止旧服务实例"
    launchctl bootout "$GUI_DOMAIN/$LABEL" 2>/dev/null || true
fi
# 清理孤儿进程: bootout 只杀 launchd 托管实例, 但前台跑过 (如 dev_mac.sh r) 或上次
# bootout 漏网的旧 wind_input 会继续占着 bridge socket, 导致新实例起来后抢不到
# socket 立即退出 (表现为 launchd 每 10s 重启、新代码看似没生效). 按安装路径精确匹配,
# 避免误杀同名的其它二进制.
# 按 service 目录匹配 (不写死文件名): 同时覆盖新中文名与旧 wind_input 残留, 且只命中
# 本服务目录下的二进制, 不误杀同名其它进程.
if pgrep -f "$INSTALL_ROOT/" >/dev/null 2>&1; then
    info "清理残留的旧服务进程"
    pkill -f "$INSTALL_ROOT/" 2>/dev/null || true
    sleep 1
fi
# 删旧文件名残留 (从 wind_input 升级到中文名时), 避免目录里两个二进制并存.
rm -f "$INSTALL_ROOT/wind_input"

# 2. 复制二进制 + 词库 (data/ 用 rsync --delete 保证与源一致, 删掉旧版残留词库).
mkdir -p "$INSTALL_ROOT" "$LOG_DIR" "$HOME/Library/LaunchAgents"
# 统一安装为「清风输入法服务」(即便 debug 源是 wind_input_debug), plist 路径稳定。
cp -f "$SRC_EXE" "$INSTALL_EXE"
chmod +x "$INSTALL_EXE"
# VM 侧 ad-hoc 重签: 跨机部署到同一路径时, 内核 amfi 会缓存上一版二进制的 cdhash,
# 新二进制 (cdhash 不同) 经 launchd 启动时缓存失配, 触发 OS_REASON_CODESIGNING 起不来.
# 原地 --force 重签生成全新签名, 刷新校验. Go 二进制本就自带 ad-hoc 签名, 这里幂等.
if command -v codesign >/dev/null; then
    codesign --force -s - "$INSTALL_EXE" 2>/dev/null \
        && info "ad-hoc 重签服务二进制" \
        || info "codesign 重签跳过 (非致命)"
fi
if command -v rsync >/dev/null; then
    rsync -a --delete "$SRC_DATA/" "$INSTALL_ROOT/data/"
else
    rm -rf "$INSTALL_ROOT/data"; cp -R "$SRC_DATA" "$INSTALL_ROOT/data"
fi
info "已复制 服务二进制 + data/ ($(find "$INSTALL_ROOT/data" -type f | wc -l | tr -d ' ') 个数据文件)"

# 3. 写 LaunchAgent plist (RunAtLoad 开机自启 + KeepAlive 崩溃自拉起).
cat > "$PLIST" <<PLIST_EOF
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>Label</key>
    <string>$LABEL</string>
    <!-- 关联到输入法 app (理论上让登录项列表归到该 app 名下). 实测: ad-hoc 无签名的
         legacy agent 会忽略它, BTM 仍按可执行文件名显示 → 真正生效靠把二进制装为中文名
         (见上 INSTALL_EXE). 此键保留, 将来有 Developer ID 签名后可正常归并. -->
    <key>AssociatedBundleIdentifiers</key>
    <string>$ASSOC_BUNDLE</string>
    <key>ProgramArguments</key>
    <array>
        <string>$INSTALL_EXE</string>
    </array>
    <key>RunAtLoad</key>
    <true/>
    <key>KeepAlive</key>
    <true/>
    <key>ProcessType</key>
    <string>Interactive</string>
    <key>StandardOutPath</key>
    <string>$OUT_LOG</string>
    <key>StandardErrorPath</key>
    <string>$ERR_LOG</string>
</dict>
</plist>
PLIST_EOF
info "已写 $PLIST"

# 4. 加载 + 启用 + 启动 (现代 launchctl bootstrap/enable/kickstart).
launchctl bootstrap "$GUI_DOMAIN" "$PLIST" 2>/dev/null || {
    err "bootstrap 失败, 重试一次 (可能旧实例未完全退出)"
    launchctl bootout "$GUI_DOMAIN/$LABEL" 2>/dev/null || true
    launchctl bootstrap "$GUI_DOMAIN" "$PLIST"
}
launchctl enable "$GUI_DOMAIN/$LABEL" 2>/dev/null || true
launchctl kickstart -k "$GUI_DOMAIN/$LABEL" 2>/dev/null || true
info "bootstrap + enable + kickstart 完成"

# 5. 验证 (等服务起 socket).
bold "==> Verify"
for i in 1 2 3 4 5 6 7 8 9 10; do
    [[ -S "$PUSH_SOCK" ]] && break
    sleep 0.3
done
# 注意: 末尾 `|| true` 必不可少. 服务尚未起 pid 时 grep 'pid =' 无匹配返回 1,
# 叠加 set -e + pipefail 会让赋值失败直接中止本「诊断报告」段 (反而吞掉真正的错误信息).
STATE=$(launchctl print "$GUI_DOMAIN/$LABEL" 2>/dev/null | grep -E '^[[:space:]]*state =' | head -1 | sed 's/^[[:space:]]*//' || true)
PID=$(launchctl print "$GUI_DOMAIN/$LABEL" 2>/dev/null | grep -E '^[[:space:]]*pid =' | head -1 | sed 's/^[[:space:]]*//' || true)
info "launchd: ${STATE:-未知} ${PID:-}"
if [[ -S "$PUSH_SOCK" ]]; then
    info "✓ push socket 存在: $PUSH_SOCK"
else
    err "✗ push socket 未出现: $PUSH_SOCK (看 $ERR_LOG)"
fi
if [[ -s "$ERR_LOG" ]]; then
    info "err.log 尾部:"
    tail -5 "$ERR_LOG" | sed 's/^/    /'
else
    info "✓ err.log 为空"
fi

bold "==> Done"
cat <<EOF

  服务已注册为开机自启 ($LABEL).
  状态: launchctl print $GUI_DOMAIN/$LABEL | grep -E 'state|pid'
  重启: launchctl kickstart -k $GUI_DOMAIN/$LABEL
  日志: $OUT_LOG / $ERR_LOG
  卸载: scripts_mac/deploy/install_service.sh --uninstall
EOF
