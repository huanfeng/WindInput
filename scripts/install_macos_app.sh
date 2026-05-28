#!/usr/bin/env bash
# install_macos_app.sh — 把 WindInput.app 装到 /Library/Input Methods/.
#
# 需要 sudo. 装完后用户去 系统设置 → 键盘 → 输入法 → + 号 → 中文 → WindInput
# 添加一次, 后续就能在状态栏 IME 切换菜单看到.
#
# 用法:
#   scripts/install_macos_app.sh                  # 装 release build
#   scripts/install_macos_app.sh --debug          # 装 debug build (路径同)
#   scripts/install_macos_app.sh --build          # 先 build 再装
#   scripts/install_macos_app.sh --uninstall      # 卸载
set -euo pipefail

SCRIPT_DIR=$(cd "$(dirname "$0")" && pwd)
REPO_DIR=$(cd "$SCRIPT_DIR/.." && pwd)
MACOS_DIR="$REPO_DIR/wind_macos"
APP_NAME="WindInput"
APP_BUNDLE="$MACOS_DIR/build/$APP_NAME.app"
INSTALL_DIR="/Library/Input Methods"
INSTALL_APP="$INSTALL_DIR/$APP_NAME.app"

DO_BUILD=0
DO_UNINSTALL=0
BUILD_ARGS=()
for arg in "$@"; do
    case "$arg" in
        --build) DO_BUILD=1 ;;
        --debug) BUILD_ARGS+=("--debug") ;;
        --uninstall) DO_UNINSTALL=1 ;;
        *) echo "[错误] 未知参数: $arg" >&2; exit 1 ;;
    esac
done

bold() { printf "\033[1m%s\033[0m\n" "$*"; }
info() { printf "  %s\n" "$*"; }
err()  { printf "\033[31m[错误] %s\033[0m\n" "$*" >&2; }

# -------- uninstall (完整清理) --------
# 仅 rm .app 是不够的: register 守护进程残留 / HIToolbox plist 启用项 / TIS LS DB
# 缓存 / Caches & Application Support 都可能残留, 导致系统设置里出现幽灵条目.
# 这里一次清干净.
BUNDLE_ID="to.feng.inputmethod.WindInput"
if [[ $DO_UNINSTALL -eq 1 ]]; then
    bold "==> Uninstall WindInput (full purge)"

    # 1. 杀所有 WindInput 进程 (含 --register-input-source 后台守护)
    info "kill WindInput processes"
    sudo pkill -9 -f "WindInput.app/Contents/MacOS/WindInput" 2>/dev/null || true
    sudo pkill -9 -x "$APP_NAME" 2>/dev/null || true
    rm -f /tmp/wind_register.log

    # 2. 删 .app
    if [[ -d "$INSTALL_APP" ]]; then
        sudo rm -rf "$INSTALL_APP"
        info "removed $INSTALL_APP"
    else
        info "(no $INSTALL_APP)"
    fi

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

    # 4. 清缓存 / state
    for d in "$HOME/Library/Caches/WindInput" "$HOME/Library/Application Support/WindInput"; do
        if [[ -d "$d" ]]; then
            rm -rf "$d"
            info "removed $d"
        fi
    done

    # 4b. 撤销 install 阶段写入的 Gatekeeper 白名单条目 (与 install 步骤 3a 配对).
    #     spctl --add 会往 /var/db/SystemPolicy 写持久规则, 不撤会无限累积.
    if command -v spctl >/dev/null; then
        sudo spctl --remove --label "WindInputDev" 2>/dev/null || true
        info "removed Gatekeeper label WindInputDev"
    fi

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
    sudo killall -HUP cfprefsd 2>/dev/null || true
    sudo killall TextInputMenuAgent 2>/dev/null || true
    sudo killall TextInputSwitcher 2>/dev/null || true
    killall imklaunchagent 2>/dev/null || true

    # 7. 验证
    sleep 0.5
    info "verify (TIS 内 WindInput 条目):"
    if [[ -f "$SCRIPT_DIR/list_input_sources.swift" ]]; then
        local_count=$(swift "$SCRIPT_DIR/list_input_sources.swift" 2>/dev/null | grep -c "$BUNDLE_ID" || true)
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
    "$SCRIPT_DIR/build_macos_app.sh" ${BUILD_ARGS[@]+"${BUILD_ARGS[@]}"}
fi

[[ -d "$APP_BUNDLE" ]] || { err "未找到 $APP_BUNDLE, 先跑 scripts/build_macos_app.sh"; exit 1; }

# -------- install --------
bold "==> Install $APP_BUNDLE -> $INSTALL_APP"

# 1. 关掉旧实例 (IMKit 进程通常常驻; 不杀的话 cp 会被持锁)
if pgrep -x "$APP_NAME" >/dev/null; then
    info "停止旧 $APP_NAME 进程"
    sudo killall "$APP_NAME" 2>/dev/null || true
    sleep 0.5
fi

# 2. 复制 .app
sudo rm -rf "$INSTALL_APP"
sudo cp -R "$APP_BUNDLE" "$INSTALL_DIR/"
info "已复制 $INSTALL_APP"

# 3. (不再重签) build_macos_app.sh 已经签好 (用 user keychain 里的 SIGN_IDENTITY).
#    早期版本在这里跑 `sudo codesign --sign "$SIGN_IDENTITY"` 想重签, 但 root 的
#    keychain 里没用户证书, 重签静默失败让 .app 退回 linker-signed adhoc, 反而
#    破坏了 build 阶段的真证书签名 (踩过的坑: flags 显示 0x20002 adhoc+linker).
#    cp -R 已保留原签名, 直接验证即可.
info "(不重签, 沿用 build 阶段 SIGN_IDENTITY 的签名)"
codesign -dv --verbose=2 "$INSTALL_APP" 2>&1 | grep -E "Authority|flags|Signature" | sed 's/^/    /'

# 3a. 把 .app 加入 Gatekeeper 白名单. ad-hoc 签名默认被 spctl reject, 而新版
#     macOS 的 IMK 注册流程会拒绝 spctl rejected 的第三方 IME (踩过的坑: spctl
#     -a 显示 rejected → TIS 列表无本 IME). spctl --add 给本 .app 单独通行证.
#     注意: 该规则写入 /var/db/SystemPolicy 持久存在, uninstall 用同名 label --remove 撤销.
if command -v spctl >/dev/null; then
    sudo spctl --add --label "WindInputDev" "$INSTALL_APP" 2>&1 | sed 's/^/    /' || true
fi

# 4. 让系统重新发现 IME bundle.
#    macOS 改 IME plist 后, 仅 cp 进 /Library/Input Methods/ 不足以让系统刷新
#    "输入源" 列表 —— LaunchServices 用 ChangeCount 缓存 bundle 信息, 不会因为
#    .app 替换而主动失效. 必须显式跑 lsregister -f 强制重读, 才能让新字段
#    (tsInputModeCharacterRepertoireKey / ComponentInputModeDict 等) 进入索引.
#    这是 Big Sur+ 上很多自打包 IME 装完看不见的真因.

LSREGISTER="/System/Library/Frameworks/CoreServices.framework/Frameworks/LaunchServices.framework/Support/lsregister"

# 4a. 强制 lsregister 重读本 bundle 元数据 (LaunchServices DB).
if [[ -x "$LSREGISTER" ]]; then
    info "lsregister -f $INSTALL_APP"
    sudo "$LSREGISTER" -f -R "$INSTALL_APP" 2>&1 | tail -3 | sed 's/^/    /'
else
    info "(lsregister 不在标准位置, 跳过)"
fi

# 4b. 杀缓存进程, 让它们重启时按新 LS DB 重扫 /Library/Input Methods/.
sudo killall -HUP cfprefsd 2>/dev/null || true
sudo killall TextInputMenuAgent 2>/dev/null || true
sudo killall TextInputSwitcher  2>/dev/null || true
# 只发 SIGTERM (不要 -9): SIP 下这些 LaunchAgent 不能 launchctl kickstart 手动重启,
# 必须靠 launchd 在收到 SIGTERM 后自动 respawn; SIGKILL 可能让它不被重启.
killall imklaunchagent 2>/dev/null || true   # 当前用户 IMK 调度器, 不需 sudo

# 4c. 触发一次 input sources 重读
defaults read com.apple.HIToolbox AppleEnabledInputSources >/dev/null 2>&1 || true

# 4d. *关键*: 调本 .app 自身 binary 的 --register-input-source.
#     macOS Tahoe (26) 起 TIS 仅接受来自 IME 自身进程的 TISRegisterInputSource
#     调用 (校验 codesign identity 匹配 bundleID), 外部 swift CLI 调 silently
#     no-op. 与 Squirrel 的 postinstall 路径一致.
APP_EXEC="$INSTALL_APP/Contents/MacOS/WindInput"
if [[ -x "$APP_EXEC" ]]; then
    RUN_AS=( )
    if [[ -n "${SUDO_USER:-}" ]]; then
        RUN_AS=(sudo -u "$SUDO_USER")
    fi

    # 重要: register 进程必须**持续运行**才能让 TIS 注册维持 (踩过的坑: register
    # 完立刻 exit 后 mode 会被系统在几秒内清掉). 后台 fork, 主流程不阻塞.
    info "$APP_EXEC --register-input-source (后台常驻维持注册)"
    "${RUN_AS[@]}" "$APP_EXEC" --register-input-source > /tmp/wind_register.log 2>&1 &
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
            ComponentInputModeDict / LSUIElement=true (不能是 LSBackgroundOnly)
         c) codesign -dv "$INSTALL_APP" 应输出 adhoc 签名信息
         d) 注销重登一次系统 (最暴力但有效, 让 TextInputSources 全量重扫)
    2. 切到 WindInput (Ctrl+Space 或菜单栏 IME 切换)
    3. 在任意文本框敲一个字母键, 然后:

         tail -F "\$HOME/Library/Caches/WindInput/logs/wind_input.log"
         log stream --predicate 'process == "WindInput"' --info --debug

       应看到:
         Go 端 : "bridge client connected connID=N"
         IME 端: "WindInput[InputController] bridge connected"
                "WindInput[handle] ..." 或 PassThrough/Consumed 路径

  卸载:    sudo scripts/install_macos_app.sh --uninstall

EOF
