#!/usr/bin/env bash
# wind_linux 端到端测试：真 Rust 服务 + 真 fcitx5 + DBus 扮演的应用，打字断言上屏。
#
#   scripts/linux/e2e.sh              # 构建 addon 与服务后跑全部用例
#   SKIP_BUILD=1 scripts/linux/e2e.sh # 直接用现成产物
#   KEEP=1 scripts/linux/e2e.sh       # 跑完保留临时目录（看日志）
#   WIND_E2E_ONLY=skin scripts/linux/e2e.sh  # 只跑异形皮肤那组（不需要设置程序）
#
# 前置：scripts/linux/bootstrap-sdk.sh 搭好的用户前缀 SDK（fcitx5 / cmake / dbus-next）。
# 输出每条用例的 PASS/FAIL，末行「E2E PASS」或「E2E FAIL」，退出码随之。
#
# 隔离：服务的配置/数据/缓存全部落在临时目录（XDG_*_HOME），socket 走 WIND_INPUT_RUNTIME_DIR，
# fcitx5 的配置走 FCITX_CONFIG_HOME，DBus 用 dbus-run-session 起一条私有会话总线——不碰
# 本机任何真实配置，也不和并发跑的另一份 e2e 串台（socket 路径带 pid）。
set -euo pipefail

REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
eval "$("$REPO/scripts/linux/bootstrap-sdk.sh" env)"
SDK_ROOT="$WIND_LINUX_SDK/root"
MULTIARCH="$(gcc -print-multiarch 2>/dev/null || echo x86_64-linux-gnu)"
TARGET_DIR="${CARGO_TARGET_DIR:-$HOME/.cache/wi-tgt-linux-wt}"
# 构建目录可覆盖：同一工作树里并发跑多份 e2e（或拿变异版 addon 验用例）时各用各的，
# 免得一份的构建把另一份正在加载的 .so 换掉。
ADDON_BUILD="${WIND_E2E_ADDON_BUILD:-$REPO/wind_linux/build/cmake}"
SERVICE_BIN="$TARGET_DIR/debug/wind_input"
DATA_DIR="${WIND_E2E_DATA:-$REPO/build_dev/data}"
# 设置程序（兄弟仓库 wind-setting）：「从输入法打开设置」用例要真的把它拉起来。
SETTING_REPO="$(cd "$REPO/.." && pwd)/wind-setting"
SETTING_TARGET_DIR="${WIND_SETTING_TARGET_DIR:-$HOME/.cache/wi-tgt-setting-linux}"
SETTING_BIN="${WIND_E2E_SETTING:-$SETTING_TARGET_DIR/debug/wind_setting}"

# socket 路径上限 108 字节：临时目录必须短。
W="${WIND_E2E_DIR:-/tmp/wi-linux/e2e.$$}"

log() { echo "[e2e] $*"; }

# ── 构建 ─────────────────────────────────────────────────────────────
if [[ -z "${SKIP_BUILD:-}" ]]; then
    log "构建 addon…"
    cmake -S "$REPO/wind_linux" -B "$ADDON_BUILD" -G Ninja >/dev/null
    cmake --build "$ADDON_BUILD" >/dev/null
    log "构建服务（linux-host）…"
    (cd "$REPO/wind_input" && CARGO_TARGET_DIR="$TARGET_DIR" \
        cargo build -q -p wind_service --features linux-host)
    log "构建扩展宿主与假扩展…"
    (cd "$REPO/wind_input" && CARGO_TARGET_DIR="$TARGET_DIR" \
        cargo build -q -p wind_extension_host --bins --examples)
    if [[ -z "${WIND_E2E_ONLY:-}" && -z "${WIND_E2E_SETTING:-}" && -d "$SETTING_REPO" ]]; then
        log "构建设置程序（$SETTING_REPO）…"
        (cd "$SETTING_REPO" && CARGO_TARGET_DIR="$SETTING_TARGET_DIR" cargo build -q)
    fi
fi
[[ -f "$ADDON_BUILD/libwindinput.so" ]] || { echo "E2E FAIL: 缺 $ADDON_BUILD/libwindinput.so"; exit 1; }
[[ -x "$SERVICE_BIN" ]] || { echo "E2E FAIL: 缺 $SERVICE_BIN"; exit 1; }
EXTENSION_HOST_BIN="$TARGET_DIR/debug/wind_input_ext"
FAKE_EXTENSION_BIN="$TARGET_DIR/debug/examples/fake_extension"
[[ -x "$EXTENSION_HOST_BIN" && -x "$FAKE_EXTENSION_BIN" ]] || { echo "E2E FAIL: 缺扩展宿主或假扩展（$EXTENSION_HOST_BIN / $FAKE_EXTENSION_BIN）"; exit 1; }
[[ -n "${WIND_E2E_ONLY:-}" || -x "$SETTING_BIN" ]] || { echo "E2E FAIL: 缺设置程序 $SETTING_BIN（需要兄弟仓库 ../wind-setting，或用 WIND_E2E_SETTING 指定）"; exit 1; }
[[ -d "$DATA_DIR/schemas" ]] || { echo "E2E FAIL: 词库目录 $DATA_DIR 不完整（需要 build_dev/data）"; exit 1; }

# ── 准备隔离目录 ─────────────────────────────────────────────────────
rm -rf "$W"
mkdir -p "$W"/{svc,rt,xdg/config,xdg/data,xdg/cache,fcitx}
cleanup() {
    [[ -f "$W/svc.pid" ]] && kill "$(cat "$W/svc.pid")" 2>/dev/null || true
    # 自动拉起模式下服务由 addon 起，pid 不归我们；按本次独有的完整路径杀。
    pkill -f -- "^$W/svc/wind_input" 2>/dev/null || true
    pkill -f -- "^$W/svc/wind_setting" 2>/dev/null || true
    if [[ -z "${KEEP:-}" ]]; then rm -rf "$W"; else log "保留临时目录 $W"; fi
}
trap cleanup EXIT

# 服务按 exe 同目录找 data/：拷一份二进制、data 下逐项软链到词库。system.phrases.toml 例外：
# 取仓库 data/ 里的那份（build_dev/data 是构建时的拷贝，可能早于最近的短语改动，比如 Linux 的
# codl），再追加 e2e 专用的按键合成短语（keysynth_cases 用，platform 限 linux）。
cp "$SERVICE_BIN" "$W/svc/wind_input"
# 扩展宿主与服务同目录（服务按 exe 同目录找它）。
cp "$EXTENSION_HOST_BIN" "$W/svc/wind_input_ext"
mkdir -p "$W/svc/data"
for f in "$DATA_DIR"/*; do
    [[ "$(basename "$f")" == system.phrases.toml ]] || ln -s "$f" "$W/svc/data/"
done
LONG_SEQ="$(printf '"Home", %.0s' $(seq 1 64))\"Home\"" # 65 个组合：超过 addon 的单条上限 64
{
    cat "$REPO/data/system.phrases.toml"
    cat <<'PHRASES'

# ── e2e 专用（scripts/linux/e2e.sh 追加）──
[[phrases]]
code = 'zkcc'
text = '$CC("[e2e 复制]", key.tap("Ctrl+C"))'
weight = 2000
platform = 'linux'

[[phrases]]
code = 'zkta'
text = '$CC("[e2e 按 a]", key.tap("a"))'
weight = 2000
platform = 'linux'

[[phrases]]
code = 'zkte'
text = '$CC("[e2e 按 End]", key.tap("End"))'
weight = 2000
platform = 'linux'

[[phrases]]
code = 'zkhs'
text = '$CC("[e2e 按住 Shift]", key.hold("Shift"))'
weight = 2000
platform = 'linux'

[[phrases]]
code = 'zkrs'
text = '$CC("[e2e 松开 Shift]", key.release("Shift"))'
weight = 2000
platform = 'linux'

[[phrases]]
code = 'zkbad'
text = '$CC("[e2e 坏组合]", key.tap("Hyper+C"))'
weight = 2000
platform = 'linux'
PHRASES
    printf "\n[[phrases]]\ncode = 'zklong'\ntext = '\$CC(\"[e2e 超长]\", key.seq(%s))'\nweight = 2000\nplatform = 'linux'\n" "$LONG_SEQ"
} >"$W/svc/data/system.phrases.toml"
# 设置程序经一层包装：先记下 addon 传来的 argv（用例据此断言深链参数），再换成真程序。
# 它的单实例 socket 在 $XDG_RUNTIME_DIR 下，下面已指进临时目录，不会与本机开着的设置程序串台。
ln -s "$SETTING_BIN" "$W/svc/wind_setting.bin"
cat >"$W/svc/wind_setting" <<SETTING
#!/usr/bin/env bash
printf '%s\\n' "\$*" >>"$W/setting.argv"
exec "$W/svc/wind_setting.bin" "\$@"
SETTING
chmod +x "$W/svc/wind_setting"
# 用户配置：切到全拼方案（出厂默认是五笔），用例按全拼写。
mkdir -p "$W/xdg/config/WindInput"
cat >"$W/xdg/config/WindInput/config.toml" <<'EOF'
[schema]
active = "pinyin"
available = ["pinyin", "wubi86"]

# 出厂 800ms：浮层交互用例（悬停保持 / 拖动 / 右键菜单）要在气泡消失前把指针挪上去。
[ui.status]
duration = 2000

# 扩展用例：假扩展把收到的事件逐行记下，客户端据此断言。
[extensions]
enabled = true

[[extensions.list]]
id = "echo"
granted = ["key.detail", "commit", "mode"]
EOF
# 假扩展装进用户目录的 extensions/echo/，记录写到 $W/extension-echo.out。
mkdir -p "$W/xdg/config/WindInput/extensions/echo"
cp "$FAKE_EXTENSION_BIN" "$W/xdg/config/WindInput/extensions/echo/fake"
cat >"$W/xdg/config/WindInput/extensions/echo/extension.toml" <<EOF
exe = "fake"
args = ["echo", "$W/extension-echo.out", "key.detail", "commit", "mode"]
EOF
export WIND_E2E_EXTENSION_OUT="$W/extension-echo.out"
# 假剪贴板：服务写剪贴板走外部命令（X11 下 xclip），e2e 环境没有、也不该碰本机剪贴板。
# 记下每次写入（参数 + 内容），`-o` 读回最近一次文本。服务与 fcitx5（自动拉起模式下由它
# 拉服务）都继承下面导出的 PATH。
mkdir -p "$W/fakebin"
cat >"$W/fakebin/xclip" <<XCLIP
#!/usr/bin/env bash
if [[ " \$* " == *" -o "* ]]; then cat "$W/clipboard.last" 2>/dev/null; exit 0; fi
if [[ " \$* " == *" image/png "* ]]; then
    printf '== %s (%s 字节)\\n' "\$*" "\$(wc -c)" >>"$W/clipboard.log"
    exit 0
fi
cat >"$W/clipboard.last"
{ printf '== %s\\n' "\$*"; cat "$W/clipboard.last"; printf '\\n'; } >>"$W/clipboard.log"
XCLIP
chmod +x "$W/fakebin/xclip"

# fcitx5 配置：与真实用户一致——输入法组是「键盘 + 清风」两个，Fcitx5 出厂的 AltTriggerKeys
# （Shift_L）原样保留。出厂它会在引擎之前把 Shift 单击截走，由 addon 在 PreInputMethod 阶段先接管
# （见 WindEngine 的 Shift 接管）；这里不清空它，「Shift 切中英」用例就是对这条的回归。
# ActiveByDefault：新输入上下文直接处于激活态（否则每个上下文都要先发触发键）。
cat >"$W/fcitx/profile" <<'EOF'
[Groups/0]
Name=Default
Default Layout=us
DefaultIM=windinput

[Groups/0/Items/0]
Name=keyboard-us
Layout=

[Groups/0/Items/1]
Name=windinput
Layout=

[GroupOrder]
0=Default
EOF
cat >"$W/fcitx/config" <<'EOF'
[Hotkey]
EnumerateWithTriggerKeys=True

[Hotkey/TriggerKeys]

[Hotkey/ActivateKeys]

[Hotkey/DeactivateKeys]

[Behavior]
ActiveByDefault=True
ShareInputState=No
EOF

# ── 在私有会话总线里起服务 + fcitx5 + 客户端 ─────────────────────────
export W SDK_ROOT MULTIARCH ADDON_BUILD REPO
dbus-run-session -- bash -c '
set -euo pipefail
export WIND_INPUT_RUNTIME_DIR="$W/rt"
# 控制 RPC（`wind_input ui toast` 等 CLI 走它）的 socket 在 $XDG_RUNTIME_DIR 下、不认
# WIND_INPUT_RUNTIME_DIR：也指进临时目录，免得与本机真服务或并发的另一份 e2e 串台。
export XDG_RUNTIME_DIR="$W/rt"
export XDG_CONFIG_HOME="$W/xdg/config" XDG_DATA_HOME="$W/xdg/data" XDG_CACHE_HOME="$W/xdg/cache"
export TMPDIR=/tmp/wi-linux
# X 先于服务起：服务要继承 DISPLAY（剪贴板走 xclip，按 DISPLAY 选后端）。
export PATH="$W/fakebin:$PATH"
unset DISPLAY WAYLAND_DISPLAY
# 候选窗（X11）：私有 Xvfb。WIND_E2E_X11=0 关掉则只测输入通路。WIND_E2E_COMPOSITOR=1 再起
# xcompmgr，走 ARGB 真透明那条路；默认无合成器，走 XShape 抠形那条路。
XVFB=""
COMP=""
if [[ "${WIND_E2E_X11:-1}" != 0 ]]; then
    XN=$(( 60 + $$ % 30 ))
    while [[ -e /tmp/.X11-unix/X$XN ]]; do XN=$((XN + 1)); done
    Xvfb-wind ":$XN" -screen 0 1280x800x24 -nolisten tcp -xkbdir /usr/share/X11/xkb \
        >"$W/xvfb.log" 2>&1 &
    XVFB=$!
    for _ in $(seq 1 50); do [[ -e /tmp/.X11-unix/X$XN ]] && break; sleep 0.1; done
    export DISPLAY=":$XN"
    echo "[e2e] Xvfb 已就绪 (DISPLAY=$DISPLAY)"
    if [[ "${WIND_E2E_COMPOSITOR:-0}" != 0 ]]; then
        xcompmgr >"$W/xcompmgr.log" 2>&1 &
        COMP=$!
        export WIND_E2E_COMP_PID=$COMP # 「合成器起停」用例要能停掉它
        sleep 0.5
        echo "[e2e] xcompmgr 已起（ARGB 路径）"
    fi
    mkdir -p "$W/shots"
    export WIND_E2E_SHOTS="$W/shots"
fi

# 服务由 restart-service.sh 起停：用例里要验「服务重启后 addon 自愈重连」，得能在客户端
# 进程里重启它（环境变量随进程树继承，重启出来的服务与首启同一套隔离目录）。
cat >"$W/restart-service.sh" <<RESTART
#!/usr/bin/env bash
if [[ -f "$W/svc.pid" ]]; then
    kill \$(cat "$W/svc.pid") 2>/dev/null || true
    while kill -0 \$(cat "$W/svc.pid") 2>/dev/null; do sleep 0.05; done
fi
rm -f "$W/rt/bridge.sock" "$W/rt/bridge_push.sock"
"$W/svc/wind_input" >>"$W/service.stdout" 2>&1 &
echo \$! >"$W/svc.pid"
for _ in \$(seq 1 300); do
    [[ -S "$W/rt/bridge.sock" && -S "$W/rt/bridge_push.sock" ]] && exit 0
    sleep 0.1
done
exit 1
RESTART
chmod +x "$W/restart-service.sh"
if [[ "${WIND_E2E_AUTOSPAWN:-0}" != 0 ]]; then
    # 自动拉起模式：不预先起服务，验证 addon 在连不上时自己把它带起来（打包后的真实场景）。
    # 拉起的服务不归本脚本管，故不设重启用例。
    export WIND_INPUT_SERVICE="$W/svc/wind_input"
    SVC=""
    echo "[e2e] 自动拉起模式：不预启服务，由 addon 拉起 $WIND_INPUT_SERVICE"
else
    export WIND_E2E_RESTART="$W/restart-service.sh"
    "$W/restart-service.sh" || true
    SVC=$(cat "$W/svc.pid")
    for _ in $(seq 1 300); do
        [[ -S "$W/rt/bridge.sock" && -S "$W/rt/bridge_push.sock" ]] && break
        sleep 0.1
    done
    [[ -S "$W/rt/bridge.sock" ]] || { echo "E2E FAIL: 服务 30s 内没起 socket"; tail -20 "$W/service.stdout"; exit 1; }
    echo "[e2e] 服务已就绪 (pid $SVC)"
fi

export FCITX_CONFIG_HOME="$W/fcitx"
# addon 收到 settings.open 时启动它（不设则是安装路径 /usr/lib/windinput/wind_setting）。
export WIND_INPUT_SETTING="$W/svc/wind_setting"
export FCITX_ADDON_DIRS="$ADDON_BUILD:$SDK_ROOT/usr/lib/$MULTIARCH/fcitx5"
export FCITX_DATA_DIRS="$ADDON_BUILD/share/fcitx5:$SDK_ROOT/usr/share/fcitx5"
# kimpanel：客户端扮演 KDE/GNOME 的面板，点状态区动作「清风输入法设置」、听当前输入法图标
# （随中英模式切换的 windinput-zh / windinput-en）。
# 菜单空闲超时调短到 5 秒，好让「超时自动收起」这条兜底在 e2e 里跑得到（出厂 60 秒）。
export WIND_MENU_IDLE_TIMEOUT_MS=5000
# 抓指针的绝对上限调到 12 秒（出厂 120 秒），验「一直有活动也不无限抓」。
export WIND_MENU_MAX_GRAB_MS=12000
# key.hold 的最长保持时间调短到 2 秒（出厂 10 秒），验「超时自动抬起」这条兜底。
export WIND_KEY_HOLD_TIMEOUT_MS=2000
# WIND_E2E_FCITX_PRELOAD：只给 fcitx5 预加载的库（拿 ASan 版 addon 跑时填 libasan.so——它必须
# 先于一切加载；设成全局 LD_PRELOAD 会连服务和客户端一起套上）。
env ${WIND_E2E_FCITX_PRELOAD:+LD_PRELOAD="$WIND_E2E_FCITX_PRELOAD"} \
fcitx5 --disable=all --enable=keyboard,dbus,dbusfrontend,kimpanel,windinput \
    --verbose="windinput=5,default=3,key_trace=5" >"$W/fcitx5.log" 2>&1 &
FCITX=$!
for _ in $(seq 1 100); do
    gdbus call --session --dest org.freedesktop.DBus --object-path /org/freedesktop/DBus \
        --method org.freedesktop.DBus.NameHasOwner org.fcitx.Fcitx5 2>/dev/null | grep -q true && break
    sleep 0.1
done
echo "[e2e] fcitx5 已就绪 (pid $FCITX)"

set +e
"$WIND_LINUX_SDK/venv/bin/python" "$REPO/scripts/linux/e2e_client.py"
RC=$?
set -e
kill $FCITX $(cat "$W/svc.pid" 2>/dev/null) $COMP $XVFB 2>/dev/null || true
wait $FCITX 2>/dev/null || true
exit $RC
' && RC=0 || RC=$?

if [[ $RC -ne 0 ]]; then
    log "fcitx5 日志尾部："
    tail -30 "$W/fcitx5.log" 2>/dev/null || true
    log "服务日志尾部："
    tail -30 "$W/xdg/data/WindInput/logs/wind_input.1.log" 2>/dev/null || true
    echo "E2E FAIL"
    exit 1
fi
echo "E2E PASS"
