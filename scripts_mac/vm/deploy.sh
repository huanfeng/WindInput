#!/usr/bin/env bash
# deploy.sh — host→VM 一键部署: 把宿主机构建好的 Go 服务 + .app + 设置应用 rsync 到
# 目标 macOS 机, 再远程调 install_service.sh / install_app.sh / install_setting.sh 完成安装.
#
# 宿主机 (Tahoe macOS 26 + Xcode) 负责构建, 干净 VM (tart ime-dev, Sequoia) 负责验证.
#
# 目标机解析顺序 (前者优先):
#   1. 命令行参数 (位置参数, 形如 admin@192.168.64.3)
#   2. 环境变量 SSH_TARGET
#   3. 默认: admin@$(tart ip ime-dev)
#
# 用法:
#   scripts_mac/vm/deploy.sh                       # rsync + 远程装 (服务 + .app)
#   scripts_mac/vm/deploy.sh --build               # 先在宿主机 build 再部署
#   scripts_mac/vm/deploy.sh --service-only        # 只部署 Go 服务
#   scripts_mac/vm/deploy.sh --app-only            # 只部署 .app (IMKit 前端)
#   scripts_mac/vm/deploy.sh --setting             # 服务 + .app + 设置应用 (wind_setting)
#   scripts_mac/vm/deploy.sh --setting-only        # 只部署设置应用 (wind_setting.app → ~/Applications)
#   scripts_mac/vm/deploy.sh --build --setting     # 构建并部署全部三者
#   scripts_mac/vm/deploy.sh --debug               # debug variant (build_debug/)
#   scripts_mac/vm/deploy.sh --uninstall           # 远程卸载 (服务 + .app) 并验证清除
#   scripts_mac/vm/deploy.sh --uninstall --service-only  # 只卸 Go 服务
#   scripts_mac/vm/deploy.sh admin@192.168.64.9    # 显式指定目标
#   SSH_TARGET=ime-vm scripts_mac/vm/deploy.sh     # 用 ssh 别名
set -euo pipefail

SCRIPT_DIR=$(cd "$(dirname "$0")" && pwd)
REPO_DIR=$(cd "$SCRIPT_DIR/../.." && pwd)

TART_VM="${TART_VM:-ime-dev}"
STAGE_DIR="${REMOTE_STAGE:-\$HOME/wind_deploy}"   # 远程 staging (远端 shell 展开 $HOME)

DO_BUILD=0
DEBUG_VARIANT=0
DO_SERVICE=1
DO_APP=1
DO_SETTING=0          # 设置应用默认不部署 (opt-in: --setting / --setting-only)
DO_UNINSTALL=0
TARGET_ARG=""
BUILD_FLAG=()
for arg in "$@"; do
    case "$arg" in
        --build)        DO_BUILD=1 ;;
        --debug)        DEBUG_VARIANT=1; BUILD_FLAG+=("--debug") ;;
        --service-only) DO_APP=0; DO_SETTING=0 ;;
        --app-only)     DO_SERVICE=0; DO_SETTING=0 ;;
        --setting)      DO_SETTING=1 ;;
        --setting-only) DO_SERVICE=0; DO_APP=0; DO_SETTING=1 ;;
        --uninstall)    DO_UNINSTALL=1 ;;
        -*)             echo "[错误] 未知参数: $arg" >&2; exit 1 ;;
        *)              TARGET_ARG="$arg" ;;
    esac
done

bold() { printf "\n\033[1m==> %s\033[0m\n" "$*"; }
info() { printf "  %s\n" "$*"; }
err()  { printf "\033[31m[错误] %s\033[0m\n" "$*" >&2; }

if [[ $DEBUG_VARIANT -eq 1 ]]; then
    BUILD_SUBDIR="build_debug"; EXE_NAME="wind_input_debug"
    APP_NAME="WindInputDebug"; INSTALL_FLAG="--debug"
    SVC_LABEL="to.feng.windinput.service.debug"; SUPPORT_NAME="WindInput_debug"
    SETTING_APP_NAME="wind_setting_debug"; SETTING_DISPLAY="清风输入法设置开发版"
else
    BUILD_SUBDIR="build"; EXE_NAME="wind_input"
    APP_NAME="WindInput"; INSTALL_FLAG=""
    SVC_LABEL="to.feng.windinput.service"; SUPPORT_NAME="WindInput"
    SETTING_APP_NAME="wind_setting"; SETTING_DISPLAY="清风输入法设置"
fi
LOCAL_BUILD="$REPO_DIR/$BUILD_SUBDIR"
LOCAL_APP="$REPO_DIR/wind_macos/build/$APP_NAME.app"
LOCAL_SETTING="$REPO_DIR/wind_setting/build/bin/$SETTING_APP_NAME.app"

# -------- 解析目标 --------
if [[ -n "$TARGET_ARG" ]]; then
    SSH_TARGET="$TARGET_ARG"
elif [[ -n "${SSH_TARGET:-}" ]]; then
    : # 用环境变量
else
    command -v tart >/dev/null || { err "tart 未安装且未指定目标 (传参数或设 SSH_TARGET)"; exit 1; }
    VM_IP=$(tart ip "$TART_VM" 2>/dev/null || true)
    [[ -n "$VM_IP" ]] || { err "tart ip $TART_VM 解析失败 (VM 没在跑?)"; exit 1; }
    SSH_TARGET="admin@$VM_IP"
fi
bold "目标: $SSH_TARGET"

# -------- 可选: 宿主机构建 --------
if [[ $DO_BUILD -eq 1 && $DO_UNINSTALL -eq 0 ]]; then
    # host 构建一律 ad-hoc 签名: 固定证书 (SIGN_IDENTITY) 只装在目标 VM, host 上没有,
    # 传给 build 脚本会 "no identity found"。证书重签在 VM 端 install_*.sh 完成 (见下方
    # SSH 调用透传 SIGN_IDENTITY/SIGN_KEYCHAIN_PW)。故 build 阶段显式清空 SIGN_IDENTITY。
    if [[ $DO_SERVICE -eq 1 ]]; then
        bold "宿主机构建 Go 服务"
        SIGN_IDENTITY= "$REPO_DIR/scripts_mac/build/build.sh" service ${BUILD_FLAG[@]+"${BUILD_FLAG[@]}"} 2>&1 | tail -5 | sed 's/^/  /'
    fi
    if [[ $DO_APP -eq 1 ]]; then
        bold "宿主机构建 $APP_NAME.app"
        SIGN_IDENTITY= "$REPO_DIR/scripts_mac/build/app.sh" ${BUILD_FLAG[@]+"${BUILD_FLAG[@]}"} 2>&1 | tail -8 | sed 's/^/  /'
    fi
    if [[ $DO_SETTING -eq 1 ]]; then
        bold "宿主机构建 $SETTING_APP_NAME.app (Wails)"
        SIGN_IDENTITY= "$REPO_DIR/scripts_mac/build/setting.sh" $INSTALL_FLAG 2>&1 | tail -6 | sed 's/^/  /'
    fi
fi

# -------- 前置检查 (卸载不需要本地产物) --------
if [[ $DO_UNINSTALL -eq 0 ]]; then
    if [[ $DO_SERVICE -eq 1 ]]; then
        [[ -f "$LOCAL_BUILD/$EXE_NAME" ]] || { err "缺 $LOCAL_BUILD/$EXE_NAME, 先跑 scripts_mac/build/build.sh 或加 --build"; exit 1; }
        [[ -d "$LOCAL_BUILD/data" ]]      || { err "缺 $LOCAL_BUILD/data, 先跑 scripts_mac/build/build.sh data 或加 --build"; exit 1; }
    fi
    if [[ $DO_APP -eq 1 ]]; then
        [[ -d "$LOCAL_APP" ]] || { err "缺 $LOCAL_APP, 先跑 scripts_mac/build/app.sh 或加 --build"; exit 1; }
    fi
    if [[ $DO_SETTING -eq 1 ]]; then
        [[ -d "$LOCAL_SETTING" ]] || { err "缺 $LOCAL_SETTING, 先跑 scripts_mac/build/setting.sh 或加 --build"; exit 1; }
    fi
fi

SSH() { ssh -o ConnectTimeout=10 "$SSH_TARGET" "$@"; }

# -------- 探活 --------
bold "1. 探活 SSH"
SSH 'echo "  ok: $(hostname) $(sw_vers -productVersion)"' || { err "SSH 连不上 $SSH_TARGET"; exit 1; }

# 远端 staging 镜像仓库结构 (含 scripts_mac/ 两级深度), 让 install 脚本的 REPO_DIR
# ($SCRIPT_DIR/../..) 自解析到 <stage>.
#   <stage>/scripts_mac/deploy/{install_service.sh, install_app.sh}
#   <stage>/scripts_mac/test/list_input_sources.swift
#   <stage>/build/{wind_input, data/}
#   <stage>/wind_macos/build/WindInput.app
bold "2. 准备远端 staging $STAGE_DIR"
SSH "mkdir -p $STAGE_DIR/scripts_mac/deploy $STAGE_DIR/scripts_mac/test $STAGE_DIR/build $STAGE_DIR/wind_macos/build $STAGE_DIR/wind_setting/build/bin"

bold "3. rsync 脚本"
rsync -az "$REPO_DIR/scripts_mac/deploy/install_service.sh" "$REPO_DIR/scripts_mac/deploy/install_app.sh" \
    "$REPO_DIR/scripts_mac/deploy/install_setting.sh" "$REPO_DIR/scripts_mac/deploy/setup_signing.sh" \
    "$SSH_TARGET:$STAGE_DIR/scripts_mac/deploy/"
rsync -az "$REPO_DIR/scripts_mac/test/list_input_sources.swift" "$SSH_TARGET:$STAGE_DIR/scripts_mac/test/"

# -------- 远程卸载 + 验证清除 --------
if [[ $DO_UNINSTALL -eq 1 ]]; then
    if [[ $DO_SERVICE -eq 1 ]]; then
        bold "4. 远程卸载 Go 服务 (普通用户, 保留用户数据)"
        SSH "chmod +x $STAGE_DIR/scripts_mac/deploy/install_service.sh && $STAGE_DIR/scripts_mac/deploy/install_service.sh --uninstall $INSTALL_FLAG"
    fi
    if [[ $DO_APP -eq 1 ]]; then
        bold "5. 远程卸载 .app (用户域, 完整清理 plist/缓存/守护进程)"
        SSH "chmod +x $STAGE_DIR/scripts_mac/deploy/install_app.sh && $STAGE_DIR/scripts_mac/deploy/install_app.sh --uninstall $INSTALL_FLAG"
    fi
    if [[ $DO_SETTING -eq 1 ]]; then
        bold "5b. 远程卸载设置应用 (普通用户, ~/Applications)"
        SSH "chmod +x $STAGE_DIR/scripts_mac/deploy/install_setting.sh && $STAGE_DIR/scripts_mac/deploy/install_setting.sh --uninstall $INSTALL_FLAG"
    fi

    bold "6. 验证清除是否干净"
    if [[ $DO_SERVICE -eq 1 ]]; then
        info "[服务]"
        SSH "L='$SVC_LABEL'; SUP='$SUPPORT_NAME'"'
             launchctl print "gui/$(id -u)/$L" >/dev/null 2>&1 \
                 && echo "  ✗ LaunchAgent 仍加载" || echo "  ✓ LaunchAgent 已 bootout"
             [ -e "$HOME/Library/LaunchAgents/$L.plist" ] \
                 && echo "  ✗ plist 残留" || echo "  ✓ plist 已删"
             [ -d "$HOME/Library/Application Support/$SUP/service" ] \
                 && echo "  ✗ service/ 目录残留" || echo "  ✓ service/ 目录已删"' \
            | sed 's/^/  /'
    fi
    if [[ $DO_APP -eq 1 ]]; then
        info "[.app]"
        SSH "A='$APP_NAME'"'
             [ -e "$HOME/Library/Input Methods/$A.app" ] \
                 && echo "  ✗ ~/Library/Input Methods/$A.app 残留" \
                 || echo "  ✓ ~/Library/Input Methods/$A.app 已删"' \
            | sed 's/^/  /'
    fi
    if [[ $DO_SETTING -eq 1 ]]; then
        info "[设置应用]"
        SSH "D='$SETTING_DISPLAY'"'
             [ -e "$HOME/Applications/$D.app" ] \
                 && echo "  ✗ ~/Applications/$D.app 残留" \
                 || echo "  ✓ ~/Applications/$D.app 已删"' \
            | sed 's/^/  /'
    fi
    info "[TIS 注册表]"
    TIS_N=$(SSH "swift $STAGE_DIR/scripts_mac/test/list_input_sources.swift 2>/dev/null | grep -c to.feng || true")
    if [[ "${TIS_N:-0}" -eq 0 ]]; then
        info "  ✓ 无 to.feng 残留条目"
    else
        info "  ✗ 仍有 $TIS_N 条 to.feng 条目 (注销重登或检查 HIToolbox plist)"
    fi

    bold "7. 卸载完成"
    info "用户数据 (config.yaml / user_data.db) 视卸载范围而定:"
    info "  仅 --service-only 卸服务时保留; 卸 .app 时 install_app.sh 会清整个 ~/Library/Application Support/WindInput"
    exit 0
fi

if [[ $DO_SERVICE -eq 1 ]]; then
    bold "4. rsync Go 服务 (二进制 + 词库)"
    # 按变体名落地 (wind_input / wind_input_debug), 与远端 install_service.sh --debug 的
    # EXE_NAME 对齐; 两变体二进制同 STAGE 共存。data/ 共享 (两变体相同)。
    rsync -az "$LOCAL_BUILD/$EXE_NAME" "$SSH_TARGET:$STAGE_DIR/build/$EXE_NAME"
    rsync -az --delete "$LOCAL_BUILD/data/" "$SSH_TARGET:$STAGE_DIR/build/data/"
fi

if [[ $DO_APP -eq 1 ]]; then
    bold "5. rsync .app"
    rsync -az --delete "$LOCAL_APP/" "$SSH_TARGET:$STAGE_DIR/wind_macos/build/$APP_NAME.app/"
fi

if [[ $DO_SETTING -eq 1 ]]; then
    bold "5b. rsync 设置应用 ($SETTING_APP_NAME.app)"
    rsync -az --delete "$LOCAL_SETTING/" "$SSH_TARGET:$STAGE_DIR/wind_setting/build/bin/$SETTING_APP_NAME.app/"
fi

# -------- 远程安装 --------
if [[ $DO_SERVICE -eq 1 ]]; then
    bold "6. 远程装 Go 服务 (LaunchAgent, 普通用户)"
    SSH "chmod +x $STAGE_DIR/scripts_mac/deploy/install_service.sh && SIGN_IDENTITY='${SIGN_IDENTITY:-}' SIGN_KEYCHAIN_PW='${SIGN_KEYCHAIN_PW:-}' $STAGE_DIR/scripts_mac/deploy/install_service.sh --from $STAGE_DIR/build $INSTALL_FLAG"
fi

if [[ $DO_APP -eq 1 ]]; then
    bold "7. 远程装 .app (用户域, 装到 ~/Library/Input Methods/)"
    # 远端 install_app.sh: 设了 SIGN_IDENTITY 则用固定证书重签 (授权持久), 否则 ad-hoc 去 runtime.
    SSH "chmod +x $STAGE_DIR/scripts_mac/deploy/install_app.sh && SIGN_IDENTITY='${SIGN_IDENTITY:-}' SIGN_KEYCHAIN_PW='${SIGN_KEYCHAIN_PW:-}' $STAGE_DIR/scripts_mac/deploy/install_app.sh $INSTALL_FLAG"
fi

if [[ $DO_SETTING -eq 1 ]]; then
    bold "7b. 远程装设置应用 (普通用户, ~/Applications)"
    SSH "chmod +x $STAGE_DIR/scripts_mac/deploy/install_setting.sh && $STAGE_DIR/scripts_mac/deploy/install_setting.sh --from $STAGE_DIR/wind_setting/build/bin $INSTALL_FLAG"
fi

bold "8. 完成"
info "服务状态: ssh $SSH_TARGET 'launchctl print gui/\$(id -u)/$SVC_LABEL | grep -E \"state|pid\"'"
info "切到 WindInput 后在文本框打字验证; 日志 ~/Library/Logs/windinput.err.log"
