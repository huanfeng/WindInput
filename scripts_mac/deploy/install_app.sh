#!/usr/bin/env bash
# install_macos_app.sh — 把 WindInput.app 装到 ~/Library/Input Methods/ (用户域).
#
# 不需要 sudo (用户域安装). 装完后用户去 系统设置 → 键盘 → 文本输入 → 编辑 → + 号
# → 简体中文 → WindInput 添加一次, 后续就能在状态栏 IME 切换菜单看到.
#
# 为何用户域 (~/Library) 而非系统域 (/Library): 实测在 macOS 26 (Tahoe) 上, 用户域 +
# ad-hoc 签名的 IME 能正常进「可添加列表」(与 Fcitx5 一致); 且无需 sudo, 也避开了
# /Library 下 root 拥有 + spctl 策略的一堆坑.
#
# 用法:
#   scripts_mac/deploy/install_app.sh                  # 装 release build
#   scripts_mac/deploy/install_app.sh --debug          # 装 debug build (路径同)
#   scripts_mac/deploy/install_app.sh --build          # 先 build 再装
#   scripts_mac/deploy/install_app.sh --uninstall      # 卸载
set -euo pipefail

SCRIPT_DIR=$(cd "$(dirname "$0")" && pwd)
REPO_DIR=$(cd "$SCRIPT_DIR/../.." && pwd)
MACOS_DIR="$REPO_DIR/wind_macos"
# 变体: release → WindInput; debug → WindInputDebug (--debug)。两变体可作为独立输入法共存。
# EXE_NAME 恒为 WindInput (= CFBundleExecutable): 两变体进程同名, 必须按 .app 路径定位进程,
# 否则装/卸 debug 会误杀正在使用的 release (反之亦然)。
APP_NAME="WindInput"
EXE_NAME="WindInput"
BUNDLE_ID="to.feng.inputmethod.WindInput"
INSTALL_DIR="$HOME/Library/Input Methods"
SRC_DIR=""

DO_BUILD=0
DO_UNINSTALL=0
BUILD_ARGS=()
while [[ $# -gt 0 ]]; do
    case "$1" in
        --build) DO_BUILD=1 ;;
        --debug) BUILD_ARGS+=("--debug"); APP_NAME="WindInputDebug"; BUNDLE_ID="to.feng.inputmethod.WindInputDebug" ;;
        --uninstall) DO_UNINSTALL=1 ;;
        # --from <dir>: 从指定目录装 (内含 <APP_NAME>.app), 供 .pkg postinstall 等离仓库场景.
        --from) shift; SRC_DIR="${1:-}"; [[ -n "$SRC_DIR" ]] || { echo "[错误] --from 缺目录参数" >&2; exit 1; } ;;
        *) echo "[错误] 未知参数: $1" >&2; exit 1 ;;
    esac
    shift
done

# 变体派生 (在 --debug/--from 解析后, 不依赖参数顺序)。
APP_BUNDLE="${SRC_DIR:-$MACOS_DIR/build}/$APP_NAME.app"
INSTALL_APP="$INSTALL_DIR/$APP_NAME.app"

bold() { printf "\033[1m%s\033[0m\n" "$*"; }
info() { printf "  %s\n" "$*"; }
err()  { printf "\033[31m[错误] %s\033[0m\n" "$*" >&2; }

# 用户域安装一律以普通用户运行 (不要 sudo): ~/Library 归属当前用户, 用 sudo 反而会让
# .app / register 进程变成 root 拥有, 引发权限错乱.
if [[ $EUID -eq 0 ]]; then
    err "请以普通用户运行 (用户域 ~/Library 安装, 不要 sudo)."
    exit 1
fi

# -------- uninstall (完整清理) --------
# 仅 rm .app 是不够的: register 守护进程残留 / HIToolbox plist 启用项 / TIS LS DB
# 缓存 / Caches & Application Support 都可能残留, 导致系统设置里出现幽灵条目.
# 这里一次清干净.
if [[ $DO_UNINSTALL -eq 1 ]]; then
    bold "==> Uninstall $APP_NAME (full purge)"

    # 1. 杀本变体的 IME 进程 (含 --register-input-source 后台守护)。按 .app 路径定位,
    #    两变体进程同名 WindInput, 不能用进程名匹配 (会误杀另一变体)。
    info "kill $APP_NAME processes"
    pkill -9 -f "$APP_NAME.app/Contents/MacOS/$EXE_NAME" 2>/dev/null || true
    rm -f /tmp/wind_register.log

    # 2. 删 .app (用户域旧路径 + 历史可能装过的系统域 /Library 都尝试清)
    for app in "$INSTALL_APP" "/Library/Input Methods/$APP_NAME.app"; do
        if [[ -d "$app" ]]; then
            if [[ -w "$(dirname "$app")" ]]; then
                rm -rf "$app" && info "removed $app"
            else
                info "(跳过 $app: 无写权限, 如需删请手动 sudo rm -rf)"
            fi
        fi
    done

    # 3. 清 HIToolbox plist 内启用项 / 选中项 (本 bundleID 相关)
    #    显式走 /usr/bin/python3 (Apple framework, plistlib 稳定);
    #    用户 PATH 上的 Homebrew python3.14 可能 libexpat ABI 不匹配, plistlib 起不来.
    info "clean HIToolbox enabled/selected entries"
    /usr/bin/python3 - <<PY
import plistlib, os, sys
path = os.path.expanduser('~/Library/Preferences/com.apple.HIToolbox.plist')
bid = "$BUNDLE_ID"
try:
    with open(path, 'rb') as f: plist = plistlib.load(f)
except FileNotFoundError:
    sys.exit(0)
changed = False
for key in ('AppleEnabledInputSources', 'AppleSelectedInputSources', 'AppleInputSourceHistory'):
    if key in plist and isinstance(plist[key], list):
        before = len(plist[key])
        plist[key] = [s for s in plist[key] if (s.get('Bundle ID') if isinstance(s, dict) else None) != bid]
        if len(plist[key]) != before:
            print(f"    {key}: {before} -> {len(plist[key])}")
            changed = True
if changed:
    with open(path, 'wb') as f: plistlib.dump(plist, f)
    print("    HIToolbox plist updated")
else:
    print("    (no HIToolbox entries matched)")
PY

    # 4. 清缓存 / state (变体目录: debug 用 Caches/WindInputDebug + App Support/WindInput_debug,
    #    与 Go buildvariant AppName()/Suffix() 对齐; release 用不带后缀的)。
    if [[ "$APP_NAME" == "WindInputDebug" ]]; then
        PURGE_DIRS=("$HOME/Library/Caches/WindInputDebug" "$HOME/Library/Application Support/WindInput_debug")
    else
        PURGE_DIRS=("$HOME/Library/Caches/WindInput" "$HOME/Library/Application Support/WindInput")
    fi
    for d in "${PURGE_DIRS[@]}"; do
        if [[ -d "$d" ]]; then
            rm -rf "$d"
            info "removed $d"
        fi
    done

    # 5. *绝不* 跑 lsregister -u / -kill (血泪教训).
    #    - lsregister -u <已删除路径>: 行为未定义, 会污染 LaunchServices DB, 导致系统设置
    #      "添加输入法" picker 对所有用户(含全新账户)报 "键盘布局不可用". 实测后果严重.
    #    - lsregister -kill -r: 新版 macOS 已移除该选项 (官方说法: dangerous & no longer useful).
    #    安全做法: .app 已删 + HIToolbox plist 已清 + cfprefsd reload, 足以让 TIS 失忆;
    #    残留 LS 索引在下次扫描自然失效. 若仍需强制刷新, 只用 `lsregister -f -R <现存路径>`
    #    (-f 重新登记, 非破坏性), 绝不对已删除路径操作.

    # 6. 重启 input source UI agents (让菜单栏 / 系统设置面板重扫).
    #    踩过的坑: killall -9 (SIGKILL) 这些 LaunchAgent 在 macOS 26 SIP 下不能
    #    用 launchctl kickstart 手动重启; 必须只发 SIGTERM, 靠 launchd 自动 respawn.
    info "restart text input agents (SIGTERM, launchd auto-respawn)"
    killall -HUP cfprefsd 2>/dev/null || true
    killall TextInputMenuAgent 2>/dev/null || true
    killall TextInputSwitcher 2>/dev/null || true
    killall imklaunchagent 2>/dev/null || true

    # 7. 验证
    sleep 0.5
    info "verify (TIS 内 WindInput 条目):"
    if [[ -f "$REPO_DIR/scripts_mac/test/list_input_sources.swift" ]]; then
        local_count=$(swift "$REPO_DIR/scripts_mac/test/list_input_sources.swift" 2>/dev/null | grep -c "$BUNDLE_ID" || true)
        info "    $local_count 条 (期望 0)"
    fi

    bold "==> Done"
    info "如果系统设置里还残留, 注销重登一次系统让 TextInputSources 全量重扫"
    exit 0
fi

# -------- build (可选) --------
if [[ $DO_BUILD -eq 1 ]]; then
    # 空数组 + set -u 在 bash 5 之前展开会报 unbound; 用 ${arr[@]+"${arr[@]}"} 形式
    # 在数组未设/空时整体不展开任何参数, 非空时正常按数组逐项展开.
    "$REPO_DIR/scripts_mac/build/app.sh" ${BUILD_ARGS[@]+"${BUILD_ARGS[@]}"}
fi

[[ -d "$APP_BUNDLE" ]] || { err "未找到 $APP_BUNDLE, 先跑 scripts_mac/build/app.sh"; exit 1; }

# -------- install --------
bold "==> Install $APP_BUNDLE -> $INSTALL_APP"

# 1. 关掉本变体的旧实例 (IMKit 进程通常常驻; 不杀的话 cp 会被持锁)。
#    按 .app 路径定位: 两变体进程同名 WindInput, 进程名匹配会误杀另一变体。
if pgrep -f "$APP_NAME.app/Contents/MacOS/$EXE_NAME" >/dev/null; then
    info "停止旧 $APP_NAME 进程"
    pkill -9 -f "$APP_NAME.app/Contents/MacOS/$EXE_NAME" 2>/dev/null || true
    sleep 0.5
fi

# 2. 复制 .app
mkdir -p "$INSTALL_DIR"
rm -rf "$INSTALL_APP"
cp -R "$APP_BUNDLE" "$INSTALL_DIR/"
info "已复制 $INSTALL_APP"

# 3. ad-hoc 产物: 就地去 hardened-runtime 重签 (实测必要).
#    build/app.sh 默认产出 `flags=0x10002(adhoc,runtime)` (ad-hoc + hardened runtime).
#    实测可正常进可添加列表的配置是「纯 ad-hoc」(flags=0x2, 无 runtime 标志, 与 Fcitx5 一致);
#    带 runtime 的 ad-hoc 在 macOS 26 上行为存疑. 这里对 ad-hoc 产物原地重签去掉 runtime 标志.
#    注: ad-hoc 重签 (`-s -`) 不涉及 keychain/证书, 普通用户即可, 幂等.
#    若 build 用了真实证书 (SIGN_IDENTITY / 已公证), 则保留原签名, 不降级成 ad-hoc.
# 检测须用 --verbose=2: 默认 -dv (verbose=1) 不打印 "Signature=adhoc" 行 (踩过的坑).
# 判据: CodeDirectory flags 里含 adhoc / 或 Signature=adhoc; 真证书则有 Authority=Developer ID.
if codesign -dv --verbose=2 "$INSTALL_APP" 2>&1 | grep -qi "adhoc"; then
    info "ad-hoc 产物: 去 hardened-runtime 重签 (codesign --force --sign -)"
    codesign --force --sign - --deep "$INSTALL_APP" 2>&1 | sed 's/^/    /' || true
else
    info "(检测到真实证书签名, 保留原签名不重签)"
fi
codesign -dv --verbose=2 "$INSTALL_APP" 2>&1 | grep -E "Authority|flags|Signature" | sed 's/^/    /'

# 3a. (已移除 spctl --add 白名单步骤 — 踩过的坑 + 证伪)
#     早期以为 ad-hoc IME 被 spctl reject 会导致 TIS 不收录, 故加 spctl --add 白名单.
#     实测证伪: Fcitx5 同为 ad-hoc + `spctl -a` rejected, 仍能正常进可添加列表; 而
#     macOS 26 (Tahoe) 已移除该能力 (`spctl --add` 报 "This operation is no longer
#     supported"). 真正决定能否进列表的是 Info.plist 不再带 tsInputModeDefaultStateKey
#     (见 wind_macos .../Resources/Info.plist 内说明), 与签名 / spctl 无关. 不再做 spctl 操作.

# 4. 让系统重新发现 IME bundle.
#    macOS 改 IME plist 后, 仅 cp 进 Input Methods/ 不足以让系统刷新 "输入源" 列表 ——
#    LaunchServices 用 ChangeCount 缓存 bundle 信息, 不会因为 .app 替换而主动失效.
#    必须显式跑 lsregister -f 强制重读, 才能让新字段 (ComponentInputModeDict 等) 进入索引.
#    这是 Big Sur+ 上很多自打包 IME 装完看不见的真因.

LSREGISTER="/System/Library/Frameworks/CoreServices.framework/Frameworks/LaunchServices.framework/Support/lsregister"

# 4a. 强制 lsregister 重读本 bundle 元数据 (LaunchServices DB).
if [[ -x "$LSREGISTER" ]]; then
    info "lsregister -f $INSTALL_APP"
    "$LSREGISTER" -f -R "$INSTALL_APP" 2>&1 | tail -3 | sed 's/^/    /'
else
    info "(lsregister 不在标准位置, 跳过)"
fi

# 4b. 杀缓存进程, 让它们重启时按新 LS DB 重扫 Input Methods/.
#    只发 SIGTERM (不要 -9): SIP 下这些 LaunchAgent 不能 launchctl kickstart 手动重启,
#    必须靠 launchd 在收到 SIGTERM 后自动 respawn; SIGKILL 可能让它不被重启.
killall -HUP cfprefsd 2>/dev/null || true
killall TextInputMenuAgent 2>/dev/null || true
killall TextInputSwitcher  2>/dev/null || true
killall imklaunchagent 2>/dev/null || true

# 4c. 触发一次 input sources 重读
defaults read com.apple.HIToolbox AppleEnabledInputSources >/dev/null 2>&1 || true

# 4d. 调本 .app 自身 binary 的 --register-input-source 立即注册 (免重启即可在 picker 出现).
#     macOS Tahoe (26) 起 TIS 仅接受来自 IME 自身进程的 TISRegisterInputSource 调用
#     (校验 codesign identity 匹配 bundleID), 外部 swift CLI 调 silently no-op.
#     (用户域无 sudo, 直接以当前用户跑.)
APP_EXEC="$INSTALL_APP/Contents/MacOS/WindInput"
if [[ -x "$APP_EXEC" ]]; then
    # 重要: register 进程保持运行以维持 TIS 注册 (踩过的坑: register 完立刻 exit 后
    # mode 可能被系统在几秒内清掉). 后台 fork, 主流程不阻塞.
    info "$APP_EXEC --register-input-source (后台常驻维持注册)"
    "$APP_EXEC" --register-input-source > /tmp/wind_register.log 2>&1 &
    REGISTER_PID=$!
    sleep 1  # 等 TIS DB 写完
    info "    PID=$REGISTER_PID (要停止后台 register: kill $REGISTER_PID)"
    head -2 /tmp/wind_register.log 2>/dev/null | sed 's/^/    /'
fi

bold "==> Done"
cat <<EOF

  下一步:
    1. 打开 系统设置 → 键盘 → 文本输入 → 编辑 → 添加 (+) → 简体中文 → 选 WindInput
       如果列表里看不到 WindInput, 按下面顺序排查:
         a) ls -la "$INSTALL_APP" 看 .app 是否真的在
         b) /usr/libexec/PlistBuddy -c "Print" "$INSTALL_APP/Contents/Info.plist" | head -40
            必须有 InputMethodConnectionName / InputMethodServerControllerClass /
            ComponentInputModeDict / LSUIElement=true (不能是 LSBackgroundOnly);
            *不应* 出现 tsInputModeDefaultStateKey (有的话该 mode 会被「+」列表过滤掉)
         c) codesign -dv "$INSTALL_APP" 应输出 adhoc 签名信息 (flags 不含 runtime)
         d) 注销重登一次系统 (最暴力但有效, 让 TextInputSources 全量重扫)
    2. 切到 WindInput (Ctrl+Space 或菜单栏 IME 切换)
    3. 在任意文本框敲一个字母键, 然后:

         tail -F "\$HOME/Library/Logs/WindInput/wind_input.log"
         log stream --predicate 'process == "WindInput"' --info --debug

       应看到:
         Go 端 : "bridge client connected connID=N"
         IME 端: "WindInput[InputController] bridge connected"
                "WindInput[handle] ..." 或 PassThrough/Consumed 路径

  卸载:    scripts_mac/deploy/install_app.sh --uninstall

EOF
