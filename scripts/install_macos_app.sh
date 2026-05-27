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

# -------- uninstall --------
if [[ $DO_UNINSTALL -eq 1 ]]; then
    bold "==> Uninstall $INSTALL_APP"
    sudo killall "$APP_NAME" 2>/dev/null || true
    sudo rm -rf "$INSTALL_APP"
    info "已删除 $INSTALL_APP"
    info "注: 系统设置里需手动移除 WindInput 输入法项 (键盘 → 输入法 → -)"
    exit 0
fi

# -------- build (可选) --------
if [[ $DO_BUILD -eq 1 ]]; then
    "$SCRIPT_DIR/build_macos_app.sh" "${BUILD_ARGS[@]}"
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
killall -9 imklaunchagent 2>/dev/null || true   # 当前用户 IMK 调度器, 不需 sudo

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
