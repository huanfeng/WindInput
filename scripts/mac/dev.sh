#!/usr/bin/env bash
# WindInput macOS 开发一站式脚本 (原生构建, 命令面对齐 Windows 的 scripts/dev.ps1)。
#
# 与 Windows 的映射: Windows 的「C++ TSF DLL (m1)」在 macOS 上对应「app 层 (WindInput.app,
# Swift/IMKit)」, 「核心 exe (m2)」对应「Rust 服务 (wind_service → wind_input 二进制)」。
# 服务才是渲染/定位/上屏的真身; app 只是 IMKit 壳。本脚本把「编译 + 生成数据 + 部署」串成
# 一套命令, 一律先编再装, 杜绝装上旧二进制。
#
# 用法:
#   scripts/mac/dev.sh              # 交互式菜单 (对齐 dev.ps1)
#   scripts/mac/dev.sh <命令> ...   # 非交互直调, 支持空格分隔连续命令 (前者失败即停)
#   scripts/mac/dev.sh menu         # 显式进菜单
#   scripts/mac/dev.sh -h|--help    # 本帮助
#
# 命令 (菜单与命令行直调同一套; 前缀 d=dev, p=部署, m=单模块):
#   1  / release      Release 全构建: service + app + gen-data 组装 + 数据校验
#   d1 / dev          Dev 全构建 (WIND_VARIANT=dev 身份)
#   m1 / dm1          仅构建 app   (WindInput.app / WindInputDev.app)  release/dev  [= Win m1 tsf]
#   m2 / dm2          仅构建 service (cargo build -p wind_service)     release/dev  [= Win m2 core]
#   m3 / dm3          仅构建 wind_setting.app (../wind-setting, 不存在则跳过) release/dev
#   p1 / pd1          系统安装全部 (service LaunchAgent + IME app + 设置 app)
#   pm1 / pdm1        仅装 app 模块   pm2 / pdm2  仅装 service   pm3 / pdm3  仅装 设置 app
#   u/u1 / ud/ud1     系统卸载 release / dev (撤销 app+service+设置+LaunchAgent, 保留用户数据)
#   8  / d8           生成 .pkg 安装包 (release / dev): 全构建后打包 (含设置 app)
#   8s / d8s          生成 .pkg (跳过重建, 直接用现有产物打包)
#   k=check  l=clippy  t=test  f=fmt  fmt-check  ci(=fmt-check+clippy+test)
#   hooks             激活 .githooks/pre-commit (提交前自动 cargo fmt --check)
#   clean
#   gd=gen-data       下载词库 + 生成 unigram/pinyin_map + 组装 data/ → build_mac/data + 校验
#   r=repl            候选 REPL (cargo run -p wind-repl -- <data>; data 默认 build_mac/data)
#
# macOS 专属便利命令:
#   run               重启 service (launchctl kickstart, 不重编)
#   logs              跟踪 service + IME 日志
#   status            诊断: service pid / socket / 签名 / 进程
#   data              把当前已装的 data/ 快照到 build_mac/data
#   sign-setup        命令行建自签证书 "WindInput Dev" (子命令: create|check|grant|remove)
#   pkg               = 8 (release .pkg); 透传 --build 等参数
#
# 全局选项:
#   --data <dir>      指定词库数据源目录 (覆盖 build_mac/data 自动解析)。
#
# 环境变量:
#   WIND_MAC_UNIVERSAL=1   产通用二进制 (arm64 + x86_64)。app / service / 设置 app 三者
#                          统一由它控制, 打包 (8) 时还会在 pkgbuild 前硬校验架构齐全。
#                          **分发包必须开**; 本地开发默认关 (单架构, 快一倍)。
#                          需 rustup 装好 x86_64-apple-darwin target (homebrew 版 rust 无 rustup)。
#   SIGN_IDENTITY="…"      指定签名身份; 显式设为空串则走 ad-hoc (CI 无证书时用)。
#                          不设则自动挑: 优先 Apple 签发的带 Team ID 证书, 回落自签 "WindInput Dev"。
#
# 变体身份 (跨 Rust/Swift 对齐, 错了 dev 变体就连不通):
#   release: app=WindInput.app  bundleID=to.feng.inputmethod.WindInput
#            数据目录=~/Library/Application Support/WindInput  LaunchAgent=to.feng.windinput.service
#   dev    : app=WindInputDev.app  bundleID=to.feng.inputmethod.WindInputDev (以 "Dev" 结尾)
#            数据目录=~/Library/Application Support/WindInputDev  LaunchAgent=to.feng.windinput.service.dev
#            LaunchAgent plist 注入 WIND_VARIANT=dev (Rust 服务据此选 WindInputDev 数据目录;
#            服务二进制用中文显示名, 文件名不以 _dev 结尾, 故必须靠环境变量声明 dev 身份)。
#
# 数据目录说明:
#   data/          源文件(入库): configs、五笔/双拼方案、主题等手工维护文件
#   .cache/        外部下载/生成(gitignore): rime-frost、opencc、pinyin-data、unigram 等
#   build_mac/     macOS 组装的运行时 data/ (gitignore); 作 service 安装数据源
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_DIR="$(cd "$SCRIPT_DIR/../.." && pwd)"
RUST_DIR="$REPO_DIR/wind_input"
MACOS_DIR="$REPO_DIR/wind_macos"
DATA_SRC="$REPO_DIR/data"               # 源文件 (入库), 组装数据的基底
CACHE_DIR="$REPO_DIR/.cache"            # 外部下载/生成 (gitignore)
DATA_SNAPSHOT="$REPO_DIR/build_mac/data"  # 组装后的运行时 data/ (gitignore); 安装数据源
SETTING_DIR="$REPO_DIR/../wind-setting"   # 设置程序 (独立项目, 不存在则跳过)

# 变体派生 (由 apply_variant 按 release/dev 设置)。
VARIANT="release"
CARGO_BUILD_ARGS=(--release)      # release → --release; dev → --profile dev-variant
PROFILE_SUBDIR="release"          # cargo target/ 下的 profile 目录名 (release / dev-variant)
APP_VARIANT_FLAG=()               # release → (); dev → (--dev)
APP_SUPPORT="$HOME/Library/Application Support/WindInput"
LABEL="to.feng.windinput.service"
INSTALLED_DATA="$APP_SUPPORT/service/data"
DATA_OVERRIDE=""

# 证书签名 (非 ad-hoc) 是必须的: macOS 26 的 IME 必须有真 Authority, 纯 ad-hoc 装上能切
# 但 IMK 不拉起控制器 → 无法输入。
#
# **优先选带 Team ID 的 Apple 签发证书 (Apple Development / Developer ID Application),
# 没有才回落自签的 "WindInput Dev"。** 差别不在能不能签, 而在 TCC 怎么记「辅助功能」授权:
#
#   带 Team ID  → TCC 存 `anchor apple generic` + subject.OU=<TeamID> 的要求, **与具体
#                 构建无关**, 重新部署后授权继续有效。
#   自签/无 OU  → TCC 只能存一条裸 `cdhash H"…"`, 钉死在当次构建上。每次重新部署 cdhash
#                 一变授权就作废 —— 而系统设置里的开关**仍显示为开**, 表现是命令直通车的
#                 按键合成、智能配对的光标回退静默不工作 (2026-08-12 实测确认)。
#
# 自签证书由 `scripts/mac/dev.sh sign-setup` 创建。可用环境变量覆盖:
#   SIGN_IDENTITY="…" 指定身份; SIGN_IDENTITY= (空) 回退 ad-hoc。
pick_sign_identity() {
    local line
    line="$(security find-identity -v -p codesigning 2>/dev/null \
        | grep -E '"(Apple Development|Developer ID Application):' | head -1)"
    if [[ -n "$line" ]]; then
        printf '%s' "$line" | sed -n 's/.*"\(.*\)".*/\1/p'
        return 0
    fi
    printf 'WindInput Dev'
}
export SIGN_IDENTITY="${SIGN_IDENTITY-$(pick_sign_identity)}"

bold() { printf "\033[1m%s\033[0m\n" "$*"; }
info() { printf "  %s\n" "$*"; }
warn() { printf "\033[33m  %s\033[0m\n" "$*"; }
err()  { printf "\033[31m[错误] %s\033[0m\n" "$*" >&2; }

# 按变体设置全局派生量。dev 服务二进制文件名不带 _dev (中文显示名), 身份靠 plist 里
# WIND_VARIANT=dev 声明; release/dev 数据目录/LaunchAgent label 均隔离, 可共存。
apply_variant() {
    local p="${1:-release}"
    if [[ "$p" == dev ]]; then
        VARIANT="dev"
        CARGO_BUILD_ARGS=(--profile dev-variant)   # 对齐 Windows Build-Core 的 dev-variant profile
        PROFILE_SUBDIR="dev-variant"
        APP_VARIANT_FLAG=(--dev)
        APP_SUPPORT="$HOME/Library/Application Support/WindInputDev"
        LABEL="to.feng.windinput.service.dev"
    else
        VARIANT="release"
        CARGO_BUILD_ARGS=(--release)
        PROFILE_SUBDIR="release"
        APP_VARIANT_FLAG=()
        APP_SUPPORT="$HOME/Library/Application Support/WindInput"
        LABEL="to.feng.windinput.service"
    fi
    INSTALLED_DATA="$APP_SUPPORT/service/data"
}

# 变体对应的 .app 目录名 (WindInput / WindInputDev)。
app_name_for_variant() { [[ "$VARIANT" == dev ]] && echo "WindInputDev" || echo "WindInput"; }

# 服务(重)启后踢掉本变体的 IME app 控制器进程，让 IMK 按需重拉新实例并重连到刚起的服务。
# 背景: 服务重启会断开 app 侧的请求 socket; app 不会自动重连该通道 (仅 push 通道靠
# SERVICE_READY 重连), 不重拉则表现为「装完/重启服务后按键无响应」。Windows 侧 TSF DLL
# 由各宿主进程自动重连, macOS 的 IMKit .app 常驻进程需显式踢一下。
# 只杀控制器实例, 保留 --register-input-source 守护 (维持 TIS 注册); IME 非当前输入源时
# 无控制器进程在跑, pgrep 落空即静默跳过。
kick_ime_app() {
    local appname pids pid args killed=0
    appname="$(app_name_for_variant)"
    pids=$(pgrep -f "$appname.app/Contents/MacOS/WindInput" 2>/dev/null) || return 0
    for pid in $pids; do
        args=$(ps -o args= -p "$pid" 2>/dev/null || true)
        case "$args" in
            *--register-input-source*) ;;                # 保留注册守护
            *) kill "$pid" 2>/dev/null && killed=1 ;;     # 只杀控制器实例
        esac
    done
    [[ $killed -eq 1 ]] && info "已重启 IME app ($appname) 控制器 — IMK 将按需重拉并重连新服务"
    return 0
}

# ───────────────────────── 子步骤: 编译 ─────────────────────────

# 通用二进制 (arm64 + x86_64) 总开关。**分发包必须开**, 本地开发默认关 (单架构快一倍)。
# app_build 自己读同一个环境变量, 故这里的全局只服务 Rust 侧 (service / wind_setting);
# 三处读同一个 WIND_MAC_UNIVERSAL, CI 在 job 级设一次即可全线贯通。
UNIVERSAL="${WIND_MAC_UNIVERSAL:-0}"

# Rust 通用二进制的落点: <项目>/target/universal/<profile>/<bin>。
#
# 为什么不 lipo 回 target/<profile>/ —— 那是原生构建的地盘。写进去以后, 「这个二进制是上次
# 原生构建的还是上次 lipo 的」就无从分辨, 而两者外观完全一样, 错拿只会在装到 Intel Mac 上
# 才暴露。'universal' 不是合法 target triple, 与 cargo 自己的 target/<triple>/ 不会撞名。
UNIVERSAL_SUBDIR="universal"

# cargo 项目的 target 目录 (产物落点)。
#
# 不能拼 "$proj/target" —— 本机若在 ~/.cargo/config.toml 里设了 build.target-dir
# (三个 Rust 项目共用一份依赖编译产物, 省下几十 G 磁盘), 或设了 CARGO_TARGET_DIR,
# 产物就根本不在项目目录内, 硬拼出来的路径会指向一个空壳或上一次的旧二进制。
# 向 cargo 自己要这个值是唯一可靠来源: 各设备的共享目录路径不同也无需改脚本,
# 没设共享时它返回的就是 <项目>/target, 与旧行为完全一致。
# cargo/jq 缺失时回落硬拼, 保证脚本在裸环境仍可用。
#
# 结果按项目缓存 (bash 3.2 无关联数组, 故用两个专用变量; macOS 自带的就是 3.2)。
_TDIR_RUST=""
_TDIR_SETTING=""
cargo_target_dir() {
    local proj="$1" d=""
    case "$proj" in
        "$RUST_DIR")    d="$_TDIR_RUST" ;;
        "$SETTING_DIR") d="$_TDIR_SETTING" ;;
    esac
    if [[ -z "$d" ]]; then
        d="$( cd "$proj" 2>/dev/null && cargo metadata --format-version 1 --no-deps 2>/dev/null \
              | jq -r '.target_directory // empty' 2>/dev/null )" || d=""
        [[ -n "$d" ]] || d="$proj/target"
        case "$proj" in
            "$RUST_DIR")    _TDIR_RUST="$d" ;;
            "$SETTING_DIR") _TDIR_SETTING="$d" ;;
        esac
    fi
    printf '%s\n' "$d"
}

# 仓库根 docs/VERSION (CI 由 tag 写入) 的规范化读取: 去 BOM 与空白, 读不到则输出空串。
# 版本真源只此一处, IME 壳 / 设置壳 / pkg / wind-setting 的 build.rs 都从这里取同一个值。
repo_version() {
    local f="$REPO_DIR/docs/VERSION"
    [[ -f "$f" ]] || return 0
    tr -d '\xef\xbb\xbf \t\r\n' < "$f"
}

# 两个 target 各编一遍再 lipo 成通用二进制。
#   用法: cargo_build_universal <项目目录> <profile子目录> <二进制名> [cargo 参数...]
# 失败一律返回非零 —— 见文件末 run_tokens 处说明, 本脚本的 errexit 不可依赖, 调用方须显式判。
cargo_build_universal() {
    local proj="$1" sub="$2" bin="$3"; shift 3
    local tdir; tdir="$(cargo_target_dir "$proj")"
    local out="$tdir/$UNIVERSAL_SUBDIR/$sub/$bin"
    local t parts=()
    for t in aarch64-apple-darwin x86_64-apple-darwin; do
        bold "==> cargo build --target $t ($bin)"
        if ! ( cd "$proj" && cargo build --target "$t" "$@" ); then
            err "通用二进制构建失败: $bin @ $t"
            err "若报「找不到 target」: rustup target add $t"
            err "(本机若是 homebrew 装的 rust 则没有 rustup, 加不了 target —— 通用构建请走 CI)"
            return 1
        fi
        parts+=("$tdir/$t/$sub/$bin")
    done
    mkdir -p "$(dirname "$out")"
    if ! lipo -create -output "$out" "${parts[@]}"; then
        err "lipo 合并失败: $bin"
        return 1
    fi
    info "universal: $out [$(lipo -archs "$out")]"
    return 0
}

build_service() {
    if [[ $UNIVERSAL -eq 1 ]]; then
        bold "==> 编译 Rust service ($VARIANT, universal)"
        cargo_build_universal "$RUST_DIR" "$PROFILE_SUBDIR" wind_input \
            "${CARGO_BUILD_ARGS[@]}" -p wind_service || return 1
    else
        bold "==> 编译 Rust service ($VARIANT)"
        ( cd "$RUST_DIR" && cargo build "${CARGO_BUILD_ARGS[@]}" -p wind_service ) || return 1
    fi
    return 0
}

# 服务二进制的路径 (随 universal 开关变)。pkg 打包与安装都从这里取, 避免两处各拼一遍。
service_bin_path() {
    local tdir; tdir="$(cargo_target_dir "$RUST_DIR")"
    if [[ $UNIVERSAL -eq 1 ]]; then
        echo "$tdir/$UNIVERSAL_SUBDIR/$PROFILE_SUBDIR/wind_input"
    else
        echo "$tdir/$PROFILE_SUBDIR/wind_input"
    fi
}

# ───────────── app_build (拼装 WindInput[Dev].app bundle) ─────────────
# SwiftPM 不直接产 .app, 这里:
#   1. swift build --product wind-input-app  (release/debug, arm64)
#   2. 按标准 macOS .app 结构拼 Contents/{MacOS, Resources, Info.plist}
#   3. codesign (自签证书或 ad-hoc, 让本机能加载)
# 输出: wind_macos/build/WindInput[Dev].app
#
# 用法:
#   app_build                  # release build + 签名
#   app_build --dev            # dev build (swift build -c debug + WindInputDev 身份)
#   app_build --no-sign        # 不 codesign (调试用)
#   app_build --universal      # arm64+x86_64 通用二进制 (分发/CI 用)
#   WIND_MAC_UNIVERSAL=1 ...    # 同上 (CI 走环境变量统一开关)
app_build() {
    # 变体: release → APP_NAME=WindInput; dev → APP_NAME=WindInputDev (--dev)。
    # .app 目录名/bundleID 按变体区分以支持共存; 可执行名 EXE_NAME 恒为 WindInput
    # (= CFBundleExecutable, 两变体同名, 仅所在 .app 路径不同)。
    local APP_BASE="WindInput"
    local VARIANT_SUFFIX=""        # dev 时 "Dev"
    local EXE_NAME="$APP_BASE"

    local SWIFT_CONFIG="release"
    local DO_SIGN=1
    # universal: arm64+x86_64 通用二进制. 环境变量 WIND_MAC_UNIVERSAL=1 或 --universal 开启.
    # 默认本机单架构 (本地/VM 快). CI 在 job 级设环境变量, 统一继承同一开关.
    local UNIVERSAL="${WIND_MAC_UNIVERSAL:-0}"
    # 默认 ad-hoc (-). 真实证书:
    #   SIGN_IDENTITY="WindInput Dev" scripts/mac/dev.sh build
    # 自签证书的创建方法见 scripts/mac/dev.sh sign-setup.
    # macOS 26 (Tahoe) 对 IME 强制要求 codesign 有真实 Authority, adhoc 被 TIS
    # 静默拒绝注册 — 本地开发期请用自签证书签名.
    SIGN_IDENTITY="${SIGN_IDENTITY:-}"
    local arg
    for arg in "$@"; do
        case "$arg" in
            --dev)       SWIFT_CONFIG="debug"; VARIANT_SUFFIX="Dev" ;;
            --no-sign)   DO_SIGN=0 ;;
            --universal) UNIVERSAL=1 ;;
            *) echo "[错误] 未知参数: $arg" >&2; exit 1 ;;
        esac
    done

    # 变体派生: APP_NAME = .app 目录名 + bundleID 后缀 (WindInput / WindInputDev)。
    local APP_NAME="${APP_BASE}${VARIANT_SUFFIX}"
    local APP_BUNDLE="$MACOS_DIR/build/$APP_NAME.app"

    command -v swift    >/dev/null || { err "swift 未安装 (装 Xcode CLT)"; exit 1; }
    command -v codesign >/dev/null || { err "codesign 未安装 (装 Xcode CLT)"; exit 1; }

    bold "==> Build wind-input-app ($SWIFT_CONFIG$([[ $UNIVERSAL -eq 1 ]] && echo ", universal"))"
    cd "$MACOS_DIR"
    local BIN_PATH PROD_SUBDIR
    if [[ $UNIVERSAL -eq 1 ]]; then
        # 多架构: SwiftPM 直接产 universal 二进制, 但落点变为 .build/apple/Products/<config>/
        # (与单架构的 .build/<config>/ 不同), 需相应取路径.
        swift build -c "$SWIFT_CONFIG" --product wind-input-app --arch arm64 --arch x86_64
        # 多架构产物落在 .build/apple/Products/<Config>/ (首字母大写). 显式映射避免 ${x^}
        # 这种 bash 4+ 语法 (macOS 自带 /bin/bash 仍是 3.2, 会报错).
        case "$SWIFT_CONFIG" in
            release) PROD_SUBDIR="Release" ;;
            debug)   PROD_SUBDIR="Debug" ;;
            *)       PROD_SUBDIR="Release" ;;
        esac
        BIN_PATH="$MACOS_DIR/.build/apple/Products/$PROD_SUBDIR/wind-input-app"
    else
        swift build -c "$SWIFT_CONFIG" --product wind-input-app
        # SwiftPM 把二进制放在 .build/<config>/wind-input-app
        BIN_PATH="$MACOS_DIR/.build/$SWIFT_CONFIG/wind-input-app"
    fi
    [[ -x "$BIN_PATH" ]] || { err "二进制未找到: $BIN_PATH"; exit 1; }
    info "binary: $BIN_PATH ($(stat -f%z "$BIN_PATH") bytes)"
    [[ $UNIVERSAL -eq 1 ]] && info "arch: $(lipo -archs "$BIN_PATH" 2>/dev/null || echo '?')"

    bold "==> Assemble $APP_BUNDLE"
    rm -rf "$APP_BUNDLE"
    mkdir -p "$APP_BUNDLE/Contents/MacOS" "$APP_BUNDLE/Contents/Resources"

    # 二进制 → Contents/MacOS/WindInput (与 Info.plist 的 CFBundleExecutable 对齐;
    # 两变体可执行同名, 仅 .app 路径不同)
    cp "$BIN_PATH" "$APP_BUNDLE/Contents/MacOS/$EXE_NAME"
    chmod +x "$APP_BUNDLE/Contents/MacOS/$EXE_NAME"

    # Info.plist
    cp "$MACOS_DIR/Sources/WindInputApp/Resources/Info.plist" "$APP_BUNDLE/Contents/Info.plist"

    # 变体注入 (dev): 全局把 bundleID 串换成 dev 变体 —— 一并改写 CFBundleIdentifier /
    # InputMethodConnectionName / ComponentInputModeDict 的 mode-id (作 dict key + TISInputSourceID
    # 值 + 有序数组项)。再把显示名 (CFBundleName/DisplayName/TISIconLabels) 加「开发版」。
    # 这样 dev .app 注册为独立输入源, 与 release 共存; 且 bundleID 以 "Dev" 结尾, Swift 的
    # BridgeEndpoints.variantSuffix 返回 "Dev" → runtimeDir=WindInputDev, 与 Rust 数据目录对齐。
    if [[ -n "$VARIANT_SUFFIX" ]]; then
        bold "==> 变体注入 (dev): bundleID/mode/连接名/显示名 → $APP_NAME"
        sed -i '' \
            -e 's/to\.feng\.inputmethod\.WindInput/to.feng.inputmethod.WindInputDev/g' \
            -e 's/清风输入法/清风输入法开发版/g' \
            "$APP_BUNDLE/Contents/Info.plist"
    fi

    # 版本贯通: 从仓库根 VERSION 文件 (CI 由 tag 写入) 注入 CFBundleShortVersionString /
    # CFBundleVersion. pkg 后续读 CFBundleShortVersionString 作 .pkg 文件名/版本/向导标题,
    # 故版本真源是 VERSION 文件. 无 VERSION 文件时保持 plist 原值 (0.0.0), 不破坏纯本地构建.
    local VERSION_FILE="$REPO_DIR/docs/VERSION"
    local APP_VERSION
    if [[ -f "$VERSION_FILE" ]]; then
        APP_VERSION=$(tr -d '\xef\xbb\xbf \t\r\n' < "$VERSION_FILE")
        if [[ -n "$APP_VERSION" ]]; then
            /usr/libexec/PlistBuddy -c "Set :CFBundleShortVersionString $APP_VERSION" "$APP_BUNDLE/Contents/Info.plist"
            /usr/libexec/PlistBuddy -c "Set :CFBundleVersion $APP_VERSION" "$APP_BUNDLE/Contents/Info.plist"
            info "version: $APP_VERSION (来自 VERSION 文件)"
        fi
    fi

    # 本地化字符串 (输入法菜单名 / 应用显示名).
    # Resources/{zh-Hans,en}.lproj/InfoPlist.strings → Contents/Resources/<lang>.lproj/InfoPlist.strings
    local lproj lang
    for lproj in "$MACOS_DIR/Sources/WindInputApp/Resources"/*.lproj; do
        [[ -d "$lproj" ]] || continue
        lang=$(basename "$lproj")
        mkdir -p "$APP_BUNDLE/Contents/Resources/$lang"
        cp -R "$lproj"/* "$APP_BUNDLE/Contents/Resources/$lang/"
        # 变体注入 (dev): mode-id 键对齐 + 本地化显示名加「开发版」/「Dev」。
        if [[ -n "$VARIANT_SUFFIX" && -f "$APP_BUNDLE/Contents/Resources/$lang/InfoPlist.strings" ]]; then
            sed -i '' \
                -e 's/to\.feng\.inputmethod\.WindInput/to.feng.inputmethod.WindInputDev/g' \
                -e 's/"清风输入法"/"清风输入法开发版"/g' \
                -e 's/"WindInput"/"WindInputDev"/g' \
                "$APP_BUNDLE/Contents/Resources/$lang/InfoPlist.strings"
        fi
        info "lproj: $lang"
    done

    # 菜单栏图标 (单色 PDF 模板). plist 引用 menu_icon.pdf, 另带 _15 / _26 应对 Retina.
    # 源 SVG 在 Resources/wind-{15,26}.svg, 重新生成: rsvg-convert -f pdf -o menu_icon_15.pdf wind-15.svg
    local icon src
    for icon in menu_icon.pdf menu_icon_15.pdf menu_icon_26.pdf; do
        src="$MACOS_DIR/Sources/WindInputApp/Resources/$icon"
        if [[ -f "$src" ]]; then
            cp "$src" "$APP_BUNDLE/Contents/Resources/$icon"
            info "icon: $icon"
        else
            err "icon missing: $src (re-generate via rsvg-convert)"
            exit 1
        fi
    done

    # 应用图标 (.icns, Finder/安装器/关于面板). plist 经 CFBundleIconFile=AppIcon 引用.
    # 源 wind_setting/build/appicon.png (1024²), 重新生成 Resources/AppIcon.icns:
    #   ICONSET=$(mktemp -d)/AppIcon.iconset; mkdir -p "$ICONSET"
    #   for s in 16 32 128 256 512; do sips -z $s $s appicon.png --out "$ICONSET/icon_${s}x${s}.png"; \
    #     sips -z $((s*2)) $((s*2)) appicon.png --out "$ICONSET/icon_${s}x${s}@2x.png"; done
    #   iconutil -c icns "$ICONSET" -o wind_macos/Sources/WindInputApp/Resources/AppIcon.icns
    local APPICON="$MACOS_DIR/Sources/WindInputApp/Resources/AppIcon.icns"
    if [[ -f "$APPICON" ]]; then
        cp "$APPICON" "$APP_BUNDLE/Contents/Resources/AppIcon.icns"
        info "icon: AppIcon.icns"
    else
        err "AppIcon.icns missing: $APPICON (从 appicon.png 经 sips+iconutil 生成)"
        exit 1
    fi

    # 写一个空的 PkgInfo (传统 macOS 期望)
    printf "APPL????" > "$APP_BUNDLE/Contents/PkgInfo"

    # 校验 Info.plist
    plutil -lint "$APP_BUNDLE/Contents/Info.plist" >/dev/null

    # Ad-hoc 签名 + Hardened Runtime (本机加载够用).
    #
    # macOS Sequoia/Tahoe (26.x) 对未启用 hardened runtime 的第三方 IME 直接静默
    # 拒绝注册到 TIS, 即使 .app 已放进 /Library/Input Methods/. 对照 Qingg.app
    # (flags=0x10000 含 runtime) 与我们裸 ad-hoc (flags=0x2) 的 codesign 差异验证.
    # --options runtime 与 --sign - (ad-hoc) 可共存, 不需要 Developer ID 证书.
    if [[ $DO_SIGN -eq 1 ]]; then
        local ENTS="$MACOS_DIR/Sources/WindInputApp/Resources/WindInput.entitlements"
        local SIGN_ARGS
        if [[ -n "$SIGN_IDENTITY" ]]; then
            bold "==> codesign with identity \"$SIGN_IDENTITY\" + hardened runtime"
            SIGN_ARGS=(--force --sign "$SIGN_IDENTITY" --options runtime --timestamp=none)
        else
            # 纯 ad-hoc, **不加 --options runtime**: 实测「带 runtime 的 ad-hoc」IME 在 macOS 26
            # 上 IMK 不拉起控制器 → 装上能切但无法输入 (见 install 段说明 / commit 6a2c21a)。
            # hardened runtime 仅在真证书 (SIGN_IDENTITY) 路径配, 见上分支。
            bold "==> codesign ad-hoc (纯, 无 hardened runtime; 真证书请用 SIGN_IDENTITY)"
            SIGN_ARGS=(--force --sign - --timestamp=none)
        fi
        if [[ -f "$ENTS" ]]; then
            SIGN_ARGS+=(--entitlements "$ENTS")
        fi
        # ⚠️ 必须显式判退出码 —— 本脚本里 errexit 是失效的 (见文件末 run_tokens 处说明)。
        # 且签名失败**不可**放过: codesign 失败会原样留下链接器的 ad-hoc 签名, 装上去的
        # 表现是「能切过去但打不出字」(IMK 不拉起控制器), 比构建直接失败难查得多。
        if ! codesign "${SIGN_ARGS[@]}" "$APP_BUNDLE"; then
            err "codesign 失败: $APP_BUNDLE"
            if [[ -n "$SIGN_IDENTITY" ]]; then
                err "常见成因与对策:"
                err "  · errSecInternalComponent = codesign 拿不到私钥。多为 login keychain"
                err "    未授权非交互访问 → 跑 'scripts/mac/dev.sh sign-setup grant'"
                err "    (在 ssh/无 tty 会话里也会这样, 换本机 Terminal 再试)"
                err "  · 证书不存在 → 'scripts/mac/dev.sh sign-setup create'"
                err "  · 现有身份 → 'scripts/mac/dev.sh sign-setup check'"
                err "  · 只想先出个能跑单测的产物: SIGN_IDENTITY= scripts/mac/dev.sh m1 (ad-hoc)"
            fi
            err "拒绝产出 ad-hoc 冒充的 .app —— 那个装上去能切换但无法输入。"
            return 1
        fi
        # ⚠️ 这里显示的签名信息**可能是旧的**: 紧跟 `--force --sign` 之后查询, securityd
        # 往往还给的是缓存里上一版的结果 (典型表现: 明明签成功了却显示 adhoc,linker-signed,
        # 隔几十秒再查就正常)。**不要**据此判定签名成败——签成没签成以上面 codesign 的
        # 退出码为准, 那才是权威信号 (SIGN_IDENTITY 非空且非 "-" 时, 退 0 即已用该身份签成)。
        # 曾在此加过一道「输出里必须有 Authority= 否则报错」的复核, 结果正是被这个缓存误杀。
        codesign -dv --verbose=2 "$APP_BUNDLE" 2>&1 | sed 's/^/    /' | head -12
    fi

    bold "==> Done"
    info "Bundle: $APP_BUNDLE"
    info "下一步: scripts/mac/dev.sh ${APP_VARIANT_FLAG[*]:+dm1}${APP_VARIANT_FLAG[*]:-pm1}  (装 app 模块)"
}

build_app() {
    bold "==> 编译 Swift .app ($VARIANT)"
    app_build ${APP_VARIANT_FLAG[@]+"${APP_VARIANT_FLAG[@]}"}
}

# ───────────── build_setting (拼装 清风输入法设置[开发版].app) ─────────────
# wind_setting 是独立 Rust 项目 (../wind-setting, windui GUI 框架); 原生 cargo build 产 bare
# 二进制, 这里按标准 macOS .app 结构拼壳 (Info.plist + AppIcon.icns + codesign)。所有资源
# (品牌图标/manifest/svg/png) 已在二进制内 include_bytes! 嵌入, 故壳内只需 二进制 + plist + icns。
#
# IME 经 LaunchServices 按 bundleID 拉起设置 app (见 Swift ModeStatusController.openSettings):
#   release IME → com.wails.wind_setting        release 设置 app
#   dev     IME → com.wails.wind_setting_debug  dev 设置 app  (Swift 判 bundleID 以 "Dev" 结尾)
# 故壳的 CFBundleIdentifier 必须与之对齐, 否则 IME 的'设置…'找不到程序。
#
# 变体身份 (连对服务): dev 壳的 CFBundleExecutable/二进制名带 _dev 后缀 → wind_setting 的
#   mode::detect 规则3 (exe 名 _dev) 命中 → 连 WindInputDev 服务 (无需注入 WIND_VARIANT);
#   与 Windows 的 wind_setting_dev.exe 模型一致。release 壳用 wind_setting → 连正式服务。
#   两变体 bundleID/文件名/壳名均隔离, 可共存。
#
# 独立仓库不存在 → 告警跳过 (返回 0, 对齐 dev.ps1 Build-Setting 的 skip-if-absent);
# cargo 构建失败 → 返回 1 (显式 m3/dm3 报错; do_full/install_all 以 best-effort 包裹不中断)。
# 输出: wind_macos/build/<APP_NAME>.app  (APP_NAME = 清风输入法设置 / 清风输入法设置开发版)
build_setting() {
    if [[ ! -d "$SETTING_DIR" ]]; then
        warn "wind_setting: ../wind-setting 不存在, 跳过 (对齐 dev.ps1 skip-if-absent)"
        return 0
    fi
    command -v codesign >/dev/null || { err "codesign 未安装 (装 Xcode CLT)"; return 1; }

    # 版本注入: docs/VERSION → wind-setting 的 build.rs (对齐 Windows 侧 dev.ps1 的
    # $env:WIND_APP_VERSION)。缺了这一步, build.rs 会回落到 git_version() —— 而那是在
    # wind-setting 自己的仓库里 describe, CI 的浅 checkout 无 tag, 结果就是界面上显示
    # 一串裸短 hash, 与本仓 tag 完全无关。
    local ver; ver="$(repo_version)"
    if [[ -n "$ver" ]]; then
        export WIND_APP_VERSION="$ver"
        info "version: $ver (注入 wind_setting build.rs)"
    fi

    # 变体派生。
    local disp exe_name bid cargo_sub
    local cargo_flags=()
    if [[ "$VARIANT" == dev ]]; then
        disp="清风输入法设置开发版"; exe_name="wind_setting_dev"
        bid="com.wails.wind_setting_debug"; cargo_sub="debug"; cargo_flags=()
    else
        disp="清风输入法设置"; exe_name="wind_setting"
        bid="com.wails.wind_setting"; cargo_sub="release"; cargo_flags=(--release)
    fi
    local APP_BUNDLE="$MACOS_DIR/build/$disp.app"

    # 1. cargo build (wind_setting package → target/[universal/]<sub>/wind_setting)。
    local BIN_PATH
    if [[ $UNIVERSAL -eq 1 ]]; then
        bold "==> 编译 wind_setting ($VARIANT, universal)"
        cargo_build_universal "$SETTING_DIR" "$cargo_sub" wind_setting \
            ${cargo_flags[@]+"${cargo_flags[@]}"} || {
            err "wind_setting 通用构建失败 (见上; 非致命, 设置 app 将缺失)"; return 1
        }
        BIN_PATH="$(cargo_target_dir "$SETTING_DIR")/$UNIVERSAL_SUBDIR/$cargo_sub/wind_setting"
    else
        bold "==> 编译 wind_setting ($VARIANT, native)"
        ( cd "$SETTING_DIR" && cargo build ${cargo_flags[@]+"${cargo_flags[@]}"} ) || {
            err "wind_setting 构建失败 (见上; 非致命, 设置 app 将缺失)"; return 1
        }
        BIN_PATH="$(cargo_target_dir "$SETTING_DIR")/$cargo_sub/wind_setting"
    fi
    [[ -x "$BIN_PATH" ]] || { err "未找到 wind_setting 二进制: $BIN_PATH"; return 1; }
    info "binary: $BIN_PATH ($(stat -f%z "$BIN_PATH") bytes)"

    # 2. 组 .app 壳。
    bold "==> Assemble $APP_BUNDLE"
    rm -rf "$APP_BUNDLE"
    mkdir -p "$APP_BUNDLE/Contents/MacOS" "$APP_BUNDLE/Contents/Resources"
    cp "$BIN_PATH" "$APP_BUNDLE/Contents/MacOS/$exe_name"
    chmod +x "$APP_BUNDLE/Contents/MacOS/$exe_name"

    # 版本: 上面读到的 docs/VERSION (无则 0.0.0), 与 IME app / pkg / 界面版本号贯通。
    [[ -n "$ver" ]] || ver="0.0.0"

    # Info.plist (窗口应用: 不设 LSUIElement; NSPrincipalClass=NSApplication)。
    cat > "$APP_BUNDLE/Contents/Info.plist" <<PLIST_EOF
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>CFBundleName</key>
    <string>$disp</string>
    <key>CFBundleDisplayName</key>
    <string>$disp</string>
    <key>CFBundleIdentifier</key>
    <string>$bid</string>
    <key>CFBundleExecutable</key>
    <string>$exe_name</string>
    <key>CFBundlePackageType</key>
    <string>APPL</string>
    <key>CFBundleShortVersionString</key>
    <string>$ver</string>
    <key>CFBundleVersion</key>
    <string>$ver</string>
    <key>CFBundleIconFile</key>
    <string>AppIcon</string>
    <!-- 与 IME app 的 Info.plist、Package.swift 的 platforms 对齐到 12.0: 整套产品的下限
         由 IME (Swift, .macOS(.v12) 编译期强制) 决定, 设置 app 单独声明更低没有意义 ——
         pkg 把两者一起装, 系统只会放行一个装得上却用不了的组合。原值 11.0 无实测依据。 -->
    <key>LSMinimumSystemVersion</key>
    <string>12.0</string>
    <key>NSHighResolutionCapable</key>
    <true/>
    <key>NSPrincipalClass</key>
    <string>NSApplication</string>
    <!-- windinput:// 协议(主题/方案/词库一键导入)。Windows 靠注册表把 URL 塞进 argv,
         macOS 靠这段声明 + windui 的 GURL Apple Event 处理器(platform/macos/url_scheme.rs)
         —— 两者缺一不可: 只声明不接事件, 点链接毫无反应(URL 不进 argv)。
         两变体共用同一 scheme, 与 Windows 一样是"谁最后注册谁接管"; 单机同时装两版时
         链接可能打到非预期变体上(见 wind-setting docs/online-update-plan.md 同一记述)。 -->
    <key>CFBundleURLTypes</key>
    <array>
        <dict>
            <key>CFBundleURLName</key>
            <string>$bid</string>
            <key>CFBundleTypeRole</key>
            <string>Viewer</string>
            <key>CFBundleURLSchemes</key>
            <array>
                <string>windinput</string>
            </array>
        </dict>
    </array>
</dict>
</plist>
PLIST_EOF
    plutil -lint "$APP_BUNDLE/Contents/Info.plist" >/dev/null
    printf "APPL????" > "$APP_BUNDLE/Contents/PkgInfo"

    # 3. AppIcon.icns (从 res/wind_setting_icon.png 256² 生成; 缺 png/工具则跳过图标, 非致命)。
    local ICON_PNG="$SETTING_DIR/res/wind_setting_icon.png"
    if [[ -f "$ICON_PNG" ]] && command -v iconutil >/dev/null; then
        local ICONSET_ROOT; ICONSET_ROOT="$(mktemp -d)"
        local ICONSET="$ICONSET_ROOT/AppIcon.iconset"; mkdir -p "$ICONSET"
        local s
        for s in 16 32 128 256; do
            sips -z "$s" "$s" "$ICON_PNG" --out "$ICONSET/icon_${s}x${s}.png" >/dev/null 2>&1 || true
            if (( s * 2 <= 256 )); then
                sips -z "$((s*2))" "$((s*2))" "$ICON_PNG" --out "$ICONSET/icon_${s}x${s}@2x.png" >/dev/null 2>&1 || true
            fi
        done
        if iconutil -c icns "$ICONSET" -o "$APP_BUNDLE/Contents/Resources/AppIcon.icns" 2>/dev/null; then
            info "icon: AppIcon.icns (源 res/wind_setting_icon.png)"
        else
            warn "AppIcon.icns 生成失败 (无图标, 非致命)"
        fi
        rm -rf "$ICONSET_ROOT"
    else
        warn "缺 $ICON_PNG 或 iconutil, 跳过应用图标 (非致命)"
    fi

    # 4. 签名 (与 IME/服务同策略: SIGN_IDENTITY 用固定证书, 否则纯 ad-hoc)。
    #    设置 app 非 IME, 无 TIS 注册顾虑, 不需 hardened runtime。
    if [[ -n "${SIGN_IDENTITY:-}" ]]; then
        bold "==> codesign with identity \"$SIGN_IDENTITY\""
        codesign --force --sign "$SIGN_IDENTITY" --timestamp=none "$APP_BUNDLE" 2>&1 | sed 's/^/    /' | head -6 || true
    else
        bold "==> codesign ad-hoc"
        codesign --force --sign - --timestamp=none "$APP_BUNDLE" 2>&1 | sed 's/^/    /' | head -6 || true
    fi

    bold "==> Done"
    info "Bundle: $APP_BUNDLE  (bundleID=$bid, exec=$exe_name)"
    return 0
}

# ───────────── setting_install (装 设置 .app 到 ~/Applications) ─────────────
# 用户域安装 (不需 sudo)。装完 lsregister 强制重读, 使 IME 的 NSWorkspace 按 bundleID 秒查到。
# 两变体 bundleID/壳名不同, 可共存。best-effort: 壳不存在 (设置仓缺/未构建) → 告警跳过, 不失败。
#
# 参数:
#   (无)          装 release 设置 app
#   --dev         装 dev 设置 app (清风输入法设置开发版.app)
#   --uninstall   卸载
#   --from <dir>  从指定目录装 (内含 <APP_NAME>.app), 供 .pkg postinstall 场景
setting_install() {
    local dev=0 do_uninstall=0 src_dir=""
    while [[ $# -gt 0 ]]; do
        case "$1" in
            --dev)       dev=1 ;;
            --uninstall) do_uninstall=1 ;;
            --from)      shift; src_dir="${1:-}"; [[ -n "$src_dir" ]] || { echo "[错误] --from 缺目录参数" >&2; exit 1; } ;;
            *) echo "[错误] 未知参数: $1" >&2; exit 1 ;;
        esac
        shift
    done

    local disp exe_name bid
    if [[ $dev -eq 1 ]]; then
        disp="清风输入法设置开发版"; exe_name="wind_setting_dev"; bid="com.wails.wind_setting_debug"
    else
        disp="清风输入法设置"; exe_name="wind_setting"; bid="com.wails.wind_setting"
    fi
    local APP_NAME="$disp.app"
    local INSTALL_DIR="$HOME/Applications"
    local DST="$INSTALL_DIR/$APP_NAME"
    local SRC="${src_dir:-$MACOS_DIR/build}/$APP_NAME"
    local LSREGISTER="/System/Library/Frameworks/CoreServices.framework/Frameworks/LaunchServices.framework/Support/lsregister"

    if [[ $EUID -eq 0 ]]; then
        err "请以普通用户运行 (用户域 ~/Applications 安装, 不要 sudo)。"
        exit 1
    fi

    # -------- uninstall --------
    if [[ $do_uninstall -eq 1 ]]; then
        bold "==> Uninstall $APP_NAME"
        # 按壳内可执行路径精确匹配 (壳名带 .app 分隔, 不误杀另一变体)。
        pkill -9 -f "$APP_NAME/Contents/MacOS/$exe_name" 2>/dev/null || true
        if [[ -d "$DST" ]]; then
            [[ -x "$LSREGISTER" ]] && "$LSREGISTER" -u "$DST" 2>/dev/null || true
            rm -rf "$DST" && info "removed $DST"
        else
            info "(未安装 $DST)"
        fi
        return 0
    fi

    # -------- install --------
    if [[ ! -d "$SRC" ]]; then
        warn "未找到设置 app: $SRC (wind_setting 仓缺失或未构建), 跳过设置安装 (非致命)"
        return 0
    fi
    bold "==> Install $SRC -> $DST"
    # 停旧实例 (按壳内可执行路径, 避免误杀另一变体)。
    pkill -9 -f "$APP_NAME/Contents/MacOS/$exe_name" 2>/dev/null && sleep 0.3 || true
    mkdir -p "$INSTALL_DIR"
    rm -rf "$DST"
    cp -R "$SRC" "$INSTALL_DIR/"
    info "已复制 $DST"
    # 原地重签 (跨机/同路径 cdhash 缓存失配防护; ad-hoc 幂等)。
    if command -v codesign >/dev/null; then
        if [[ -n "${SIGN_IDENTITY:-}" ]]; then
            [[ -n "${SIGN_KEYCHAIN_PW:-}" ]] && security unlock-keychain -p "$SIGN_KEYCHAIN_PW" "$HOME/Library/Keychains/login.keychain-db" 2>/dev/null || true
            codesign --force --sign "$SIGN_IDENTITY" --deep "$DST" 2>/dev/null && info "固定证书重签: \"$SIGN_IDENTITY\"" || info "codesign 重签跳过 (非致命)"
        else
            codesign --force --sign - --deep "$DST" 2>/dev/null && info "ad-hoc 重签" || info "codesign 重签跳过 (非致命)"
        fi
    fi
    # lsregister 强制重读, 让 IME 的 urlForApplication(bundleID=$bid) 立即命中。
    if [[ -x "$LSREGISTER" ]]; then
        "$LSREGISTER" -f -R "$DST" 2>/dev/null || true
        info "lsregister -f -R (bundleID=$bid 可被 LaunchServices 查到)"
    fi
    bold "==> Done"
    info "设置 app 已装: $DST"
    info "IME 菜单'设置…'/候选栏会经 bundleID ($bid) 拉起它。"
    return 0
}

# 组合: 编 + 装 设置 app (best-effort: 构建失败/仓缺失不中断上层安装)。
install_setting() {
    build_setting || warn "wind_setting 构建失败/跳过 (设置 app 可能缺失)"
    bold "==> 安装 设置 app ($VARIANT)"
    setting_install ${APP_VARIANT_FLAG[@]+"${APP_VARIANT_FLAG[@]}"}
    # 防复发: 清 build/ 里的设置壳并注销其 LS 登记 (与 install_app 同策略)。它与 ~/Applications
    # 里的真身同 bundleID, 留着会被 LaunchServices 重复登记; 若日后 build/ 被清则成幽灵路径。
    # 真身已装到 ~/Applications, build/ 仅中间产物 (pkg 打包会即时重建), 可删。
    local disp="清风输入法设置"; [[ "$VARIANT" == dev ]] && disp="清风输入法设置开发版"
    local built="$MACOS_DIR/build/$disp.app"
    if [[ -d "$built" ]]; then
        local lsreg="/System/Library/Frameworks/CoreServices.framework/Frameworks/LaunchServices.framework/Support/lsregister"
        [[ -x "$lsreg" ]] && "$lsreg" -u "$built" 2>/dev/null || true
        rm -rf "$built"
        info "已清理 build/ 重复设置壳 (防 LS 重复登记)"
    fi
}

# ───────────────────────── gen-data (原生, 对齐 dev.ps1) ─────────────────────────

# 下载单个词库文件到 .cache/ (已存在则跳过; curl 重试 3 次; 失败告警但不硬断, 交 verify 兜底)。
get_dict() {
    local url="$1" dst="$2" desc="${3:-}"
    if [[ -f "$dst" ]]; then info "[skip] $(basename "$dst") 已存在"; return 0; fi
    info "[get ] $(basename "$dst") $desc"
    mkdir -p "$(dirname "$dst")"
    if curl -fsSL --retry 3 -o "$dst" "$url"; then return 0; fi
    rm -f "$dst"
    warn "下载失败: $url"
    return 0
}

# 下载 rime-frost / pinyin-data / OpenCC 词库 → .cache/。
download_dicts() {
    bold "==> 下载外部词库 → $CACHE_DIR"
    local rime="$CACHE_DIR/rime-frost" f
    local frost="https://raw.githubusercontent.com/gaboolic/rime-frost/master"
    info "rime-frost (拼音):"
    get_dict "$frost/rime_frost.dict.yaml" "$rime/rime_frost.dict.yaml" "词库入口"
    for f in 8105 41448 base ext others corrections tencent; do
        get_dict "$frost/cn_dicts/$f.dict.yaml" "$rime/cn_dicts/$f.dict.yaml"
    done
    info "rime-frost (英文):"
    for f in en en_ext; do get_dict "$frost/en_dicts/$f.dict.yaml" "$rime/en_dicts/$f.dict.yaml"; done

    local py="$CACHE_DIR/pinyin-data"
    local pybase="https://raw.githubusercontent.com/mozillazg/pinyin-data/master"
    info "pinyin-data (汉字拼音反查):"
    for f in pinyin kXHC1983 kTGHZ2013 kMandarin_8105 overwrite; do get_dict "$pybase/$f.txt" "$py/$f.txt"; done

    local oc="$CACHE_DIR/opencc/dictionaries"
    local ocbase="https://raw.githubusercontent.com/BYVoid/OpenCC/master/data/dictionary"
    info "OpenCC 简繁词典:"
    for f in STCharacters STPhrases TWVariants TWPhrases HKVariants; do get_dict "$ocbase/$f.txt" "$oc/$f.txt"; done

    # 五笔词库: 主库与 extra 由 gen_dict 重排/拆分, district 原样复制 (见 assemble_data)。
    local wubi="$CACHE_DIR/rime-wubi"
    local wubibase="https://raw.githubusercontent.com/KyleBing/rime-wubi86-jidian/master"
    info "rime-wubi86-jidian (五笔):"
    for f in wubi86_jidian wubi86_jidian_extra wubi86_jidian_extra_district; do
        get_dict "$wubibase/$f.dict.yaml" "$wubi/$f.dict.yaml"
    done

    # 辅助码表: 拼音候选的字形二次筛选 (默认关闭, 见 schema.pinyin.aux_code)。
    # 小鹤/自然码零转换; 笔画表由 gen_aux_code 从 rime-stroke 的 .dict.yaml 转换 + 字集裁剪。
    # ⚠️ rime-stroke 是 LGPL-3.0 —— 同 rime-frost 处理: 只下载不入库, 见 NOTICE.md。
    local aux="$CACHE_DIR/aux-code"
    mkdir -p "$aux/charset"
    local auxbase="https://raw.githubusercontent.com/HowcanoeWang/rime-lua-aux-code/main/aux_code"
    info "辅助码表:"
    for f in flypy_full ZRM-wanxiang; do
        get_dict "$auxbase/$f.txt" "$aux/$f.txt"
    done
    get_dict "https://raw.githubusercontent.com/rime/rime-stroke/master/stroke.dict.yaml" \
        "$aux/stroke.dict.yaml"
    # 字集 (文件名须与 gen_aux_code::CHARSET_FILES 一致; URL 段已百分号编码)
    local hanzibase="https://raw.githubusercontent.com/zispace/hanzi-chars/main"
    get_dict "$hanzibase/data-charset/GB%2018030-2000.txt" "$aux/charset/GB 18030-2000.txt"
    get_dict "$hanzibase/data-charlist/%E3%80%8A%E9%80%9A%E7%94%A8%E8%A7%84%E8%8C%83%E6%B1%89%E5%AD%97%E8%A1%A8%E3%80%8B%EF%BC%882013%E5%B9%B4%EF%BC%89.txt" \
        "$aux/charset/《通用规范汉字表》（2013年）.txt"
    get_dict "$hanzibase/data-unicode/Unicode-CJK%20%E3%80%87.txt" "$aux/charset/Unicode-CJK 〇.txt"
    return 0
}

# 从 data/(源) + .cache/(下载/生成) 组装完整运行时数据到 build_mac/data。
assemble_data() {
    local data="$DATA_SNAPSHOT"
    local pinyin="$data/schemas/pinyin"
    local pinyin_cn="$pinyin/cn_dicts"
    local english="$data/schemas/english"
    local rime="$CACHE_DIR/rime-frost"
    local f

    bold "==> 组装 data/ → $data"
    [[ -d "$DATA_SRC" ]] || { err "源数据目录不存在: $DATA_SRC"; return 1; }
    # 先清空目标再组装, 保证内容干净且包含仓库 data/ 里最新的 schemas/shuangpin/*.toml 与 toml 主题。
    rm -rf "$data"
    mkdir -p "$(dirname "$data")"

    # 1. 复制 data/ 源文件 (configs、五笔/双拼方案、主题等)。
    cp -R "$DATA_SRC" "$data"

    # 1b. 合并 wind_input/data/settings/ (manifest.toml 等 RPC 元数据; 存在才合并)。
    if [[ -d "$RUST_DIR/data/settings" ]]; then
        mkdir -p "$data/settings"
        cp -R "$RUST_DIR/data/settings/." "$data/settings/"
    fi

    # 2. rime-frost 拼音词库。
    mkdir -p "$pinyin_cn"
    if [[ -f "$rime/rime_frost.dict.yaml" ]]; then
        cp -f "$rime/rime_frost.dict.yaml" "$pinyin/"
        for f in 8105 41448 base ext others corrections; do
            [[ -f "$rime/cn_dicts/$f.dict.yaml" ]] && cp -f "$rime/cn_dicts/$f.dict.yaml" "$pinyin_cn/"
        done
    else
        warn "缺 .cache/rime-frost/, 拼音词库不可用 (先跑 gd 下载)"
    fi

    # 3. 英文词库。
    mkdir -p "$english"
    for f in en en_ext; do
        [[ -f "$rime/en_dicts/$f.dict.yaml" ]] && cp -f "$rime/en_dicts/$f.dict.yaml" "$english/"
    done

    # 4. (unigram.txt 不再随 data/ 分发: 引擎侧的读取链已移除, 词图打分改用词条自身的
    #    词典权重, 见 wind-engine/pinyin/lattice.rs::score_node_inner。
    #    .cache 里的 unigram.txt 仍由 gd 生成 —— gen_dict 用它给五笔扩展词库赋权。)

    # 4b. 汉字拼音反查表。
    local pmap="$CACHE_DIR/pinyin-data/pinyin_map.txt"
    if [[ -f "$pmap" ]]; then cp -f "$pmap" "$data/pinyin_map.txt"; else warn "缺 pinyin_map.txt (先跑 gd 生成)"; fi

    # 5. OpenCC 编译 .octrie (Rust 工具 gen_opencc)。
    mkdir -p "$data/opencc"
    if ls "$CACHE_DIR/opencc/dictionaries"/*.txt >/dev/null 2>&1; then
        info "编译 OpenCC → .octrie ..."
        ( cd "$RUST_DIR" && cargo run -q -p wind-tools --bin gen_opencc -- --src "$CACHE_DIR/opencc/dictionaries" --out "$data/opencc" ) \
            || warn "OpenCC 编译失败 (简繁转换不可用)"
    else
        warn "缺 .cache/opencc/, OpenCC 不可用 (先跑 gd 下载)"
    fi

    # 6. 五笔词库 (Rust 工具 gen_dict): 主库按词频重排 + extra 拆成 4 库。
    # 产物直接写进 build 目录、不入版本库 —— 源码树 data/schemas/wubi86/ 只保留
    # wubi86.schema.toml 与字体等真正的源文件。
    local wubi="$CACHE_DIR/rime-wubi"
    local wubi_out="$data/schemas/wubi86"
    if [[ -f "$wubi/wubi86_jidian.dict.yaml" ]]; then
        info "生成五笔词库 (gen_dict) ..."
        mkdir -p "$wubi_out"
        # district 由 gen_dict 的 passthrough 一并处理 (原样透传 + 清洗头部)
        ( cd "$RUST_DIR" && cargo run -q -p wind-tools --bin gen_dict -- \
            --cache "$CACHE_DIR" --out "$wubi_out" --report "$wubi" ) \
            || warn "五笔词库生成失败 (五笔方案不可用)"
    else
        warn "缺 .cache/rime-wubi/, 五笔词库不可用 (先跑 gd 下载)"
    fi

    # 7. 辅助码表 (Rust 工具 gen_aux_code): 小鹤/自然码原样透传, 笔画表 YAML→`字=码` + 字集裁剪。
    # 与五笔同理: 产物只进 build 目录、不入版本库 (rime-stroke 是 LGPL-3.0, 见 NOTICE.md)。
    # 功能出厂关闭, 故缺表只是「辅助码用不了」, 不影响其它一切 —— 用 warn 不中断构建。
    if [[ -f "$CACHE_DIR/aux-code/stroke.dict.yaml" ]]; then
        info "生成辅助码表 (gen_aux_code) ..."
        mkdir -p "$data/schemas/aux_code"
        ( cd "$RUST_DIR" && cargo run -q -p wind-tools --bin gen_aux_code -- \
            --cache "$CACHE_DIR" --out "$data/schemas/aux_code" ) \
            || warn "辅助码表生成失败 (辅助码功能不可用)"
    else
        warn "缺 .cache/aux-code/, 辅助码不可用 (先跑 gd 下载)"
    fi

    info "data/ 组装完成 ($(find "$data" -type f | wc -l | tr -d ' ') 文件)"
    return 0
}

# 下载外部词库 + 生成 unigram/pinyin_map + 组装 data/ → build_mac/data。
do_gendata() {
    bold "========== gen-data (下载 + 生成 + 组装) → $DATA_SNAPSHOT =========="
    download_dicts

    # 生成 unigram 词频表 (Rust 工具 gen_unigram)。仅供 gen_dict 给五笔扩展词库赋权,
    # 不随 data/ 分发 —— 引擎侧已改用词条自身的词典权重打分。
    local unigram="$CACHE_DIR/pinyin-frost/unigram.txt"
    mkdir -p "$(dirname "$unigram")"
    if [[ ! -f "$unigram" ]]; then
        bold "==> 生成 unigram 词频表"
        ( cd "$RUST_DIR" && cargo run -q -p wind-tools --bin gen_unigram -- --rime "$CACHE_DIR/rime-frost/cn_dicts" --out "$unigram" ) \
            || warn "unigram 生成失败 (gen_dict 五笔赋权将随之失败)"
    else
        info "unigram 已缓存"
    fi

    # 生成汉字拼音反查表 (Rust 工具 gen_pinyin)。
    local pmap="$CACHE_DIR/pinyin-data/pinyin_map.txt"
    if [[ -f "$CACHE_DIR/pinyin-data/pinyin.txt" ]]; then
        bold "==> 生成汉字拼音反查表"
        ( cd "$RUST_DIR" && cargo run -q -p wind-tools --bin gen_pinyin -- --src "$CACHE_DIR/pinyin-data" --out "$pmap" ) \
            || warn "拼音反查表生成失败 (候选拼音提示不可用)"
    else
        warn "缺 .cache/pinyin-data/pinyin.txt, 拼音反查表不可用"
    fi

    assemble_data
    bold "==> gen-data 完成 → $DATA_SNAPSHOT"
    return 0
}

# 发布前硬门禁: 校验关键运行时数据完整 (缺失/过小即失败)。对齐 dev.ps1 Verify-DistData。
verify_dist_data() {
    local data="$DATA_SNAPSHOT" ok=1
    bold "==> 校验发布数据完整性 → $data"
    _check_min() {
        local rel="$1" min="$2" p="$data/$1" sz
        if [[ ! -f "$p" ]]; then err "  ✗ 缺失: $rel"; ok=0; return; fi
        sz=$(stat -f%z "$p" 2>/dev/null || echo 0)
        if (( sz < min )); then err "  ✗ 过小 (${sz}B < 期望 ${min}B): $rel"; ok=0
        else info "  ✓ $rel ($((sz / 1024))KB)"; fi
    }
    _check_min "schemas/pinyin/cn_dicts/base.dict.yaml" 1000000
    _check_min "schemas/pinyin/cn_dicts/8105.dict.yaml" 10000
    _check_min "schemas/english/en.dict.yaml"           1000
    _check_min "pinyin_map.txt"                         10000
    # 五笔词库为 gen_dict 生成物、不入版本库 —— 忘跑 gd 时必须在此拦下
    _check_min "schemas/wubi86/wubi86_jidian.dict.yaml"                1000000
    _check_min "schemas/wubi86/wubi86_jidian_extra.dict.yaml"          10000
    _check_min "schemas/wubi86/wubi86_jidian_emoji.dict.yaml"          1000
    _check_min "schemas/wubi86/wubi86_jidian_extra_district.dict.yaml" 10000
    local oc; oc=$(ls "$data/opencc"/*.octrie 2>/dev/null | wc -l | tr -d ' ')
    if (( oc < 1 )); then err "  ✗ 缺失: opencc/*.octrie (简繁转换编译失败)"; ok=0
    else info "  ✓ opencc/*.octrie ($oc 个)"; fi

    if (( ok == 0 )); then
        err "发布数据校验失败! 上述文件缺失或异常会导致功能残缺。"
        err "请排查 gd 的下载/生成 (词库源、网络、gen_opencc/gen_dict)。"
        return 1
    fi
    bold "==> 发布数据校验通过"
    return 0
}

# ───────────── 数据解析 (安装 service 需要 data/) ─────────────
# 顺序: --data > build_mac/data > 当前已装 service/data (自动快照到 build_mac/data) > 报错。
resolve_data() {
    if [[ -n "$DATA_OVERRIDE" ]]; then echo "$DATA_OVERRIDE"; return; fi
    if [[ -d "$DATA_SNAPSHOT" ]]; then echo "$DATA_SNAPSHOT"; return; fi
    if [[ -d "$INSTALLED_DATA" ]]; then
        mkdir -p "$DATA_SNAPSHOT"
        cp -R "$INSTALLED_DATA/." "$DATA_SNAPSHOT/"
        warn "已把当前已装 data/ 快照到 build_mac/data (后续复用; 词库更新请重跑 gd)" >&2
        echo "$DATA_SNAPSHOT"; return
    fi
    err "找不到词库数据源 (--data / build_mac/data / 已装 service/data 均无)。"
    err "先跑 scripts/mac/dev.sh gd 组装数据, 或 --data 指定。"
    exit 1
}

# ───────────── app_install (装 .app 到 ~/Library/Input Methods/) ─────────────
# 用户域安装 (不需 sudo). 装完后系统设置 → 键盘 → 文本输入 → 编辑 → + → 简体中文 → 选 WindInput[开发版]。
#
# 参数:
#   (无)            装 release build
#   --dev           装 dev build (WindInputDev.app)
#   --build         先 build 再装
#   --uninstall     卸载
#   --from <dir>    从指定目录装 (内含 <APP_NAME>.app), 供 .pkg postinstall 等离仓库场景.
app_install() {
    # 变体: release → WindInput; dev → WindInputDev (--dev)。两变体可作为独立输入法共存。
    # EXE_NAME 恒为 WindInput (= CFBundleExecutable): 两变体进程同名, 必须按 .app 路径定位进程,
    # 否则装/卸 dev 会误杀正在使用的 release (反之亦然)。
    local APP_NAME="WindInput"
    local EXE_NAME="WindInput"
    local BUNDLE_ID="to.feng.inputmethod.WindInput"
    local INSTALL_DIR="$HOME/Library/Input Methods"
    local SRC_DIR=""

    local DO_BUILD=0
    local DO_UNINSTALL=0
    local BUILD_ARGS=()
    while [[ $# -gt 0 ]]; do
        case "$1" in
            --build) DO_BUILD=1 ;;
            --dev) BUILD_ARGS+=("--dev"); APP_NAME="WindInputDev"; BUNDLE_ID="to.feng.inputmethod.WindInputDev" ;;
            --uninstall) DO_UNINSTALL=1 ;;
            # --from <dir>: 从指定目录装 (内含 <APP_NAME>.app), 供 .pkg postinstall 等离仓库场景.
            --from) shift; SRC_DIR="${1:-}"; [[ -n "$SRC_DIR" ]] || { echo "[错误] --from 缺目录参数" >&2; exit 1; } ;;
            *) echo "[错误] 未知参数: $1" >&2; exit 1 ;;
        esac
        shift
    done

    # 变体派生 (在 --dev/--from 解析后, 不依赖参数顺序)。
    local APP_BUNDLE="${SRC_DIR:-$MACOS_DIR/build}/$APP_NAME.app"
    local INSTALL_APP="$INSTALL_DIR/$APP_NAME.app"

    # 用户域安装一律以普通用户运行 (不要 sudo): ~/Library 归属当前用户, 用 sudo 反而会让
    # .app / register 进程变成 root 拥有, 引发权限错乱.
    if [[ $EUID -eq 0 ]]; then
        err "请以普通用户运行 (用户域 ~/Library 安装, 不要 sudo)."
        exit 1
    fi

    # -------- uninstall (完整清理) --------
    # 仅 rm .app 是不够的: register 守护进程残留 / HIToolbox plist 启用项 / TIS LS DB
    # 缓存 / Caches & Application Support 都可能残留, 导致系统设置里出现幽灵条目. 一次清干净.
    if [[ $DO_UNINSTALL -eq 1 ]]; then
        bold "==> Uninstall $APP_NAME (full purge)"

        # 1. 杀本变体的 IME 进程 (含 --register-input-source 后台守护)。按 .app 路径定位,
        #    两变体进程同名 WindInput, 不能用进程名匹配 (会误杀另一变体)。
        info "kill $APP_NAME processes"
        pkill -9 -f "$APP_NAME.app/Contents/MacOS/$EXE_NAME" 2>/dev/null || true
        rm -f /tmp/wind_register.log

        # 2. 删 .app (用户域旧路径 + 历史可能装过的系统域 /Library 都尝试清)
        local app
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

        # 4. 清缓存 / state (变体目录: dev 用 Caches/WindInputDev + App Support/WindInputDev,
        #    与 Rust variant::app_dir_name() 对齐; release 用不带后缀的)。
        local PURGE_DIRS d
        if [[ "$APP_NAME" == "WindInputDev" ]]; then
            PURGE_DIRS=("$HOME/Library/Caches/WindInputDev" "$HOME/Library/Application Support/WindInputDev")
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

        bold "==> Done"
        info "如果系统设置里还残留, 注销重登一次系统让 TextInputSources 全量重扫"
        exit 0
    fi

    # -------- build (可选) --------
    if [[ $DO_BUILD -eq 1 ]]; then
        # 空数组 + set -u 在 bash 5 之前展开会报 unbound; 用 ${arr[@]+"${arr[@]}"} 形式
        # 在数组未设/空时整体不展开任何参数, 非空时正常按数组逐项展开.
        app_build ${BUILD_ARGS[@]+"${BUILD_ARGS[@]}"}
    fi

    [[ -d "$APP_BUNDLE" ]] || { err "未找到 $APP_BUNDLE, 先跑 scripts/mac/dev.sh ${BUILD_ARGS[*]:+dm1}${BUILD_ARGS[*]:-m1}"; exit 1; }

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
    #    app_build 默认产出 `flags=0x10002(adhoc,runtime)` (ad-hoc + hardened runtime).
    #    实测可正常进可添加列表 + 能被 IMK 拉起的配置是「纯 ad-hoc」(flags=0x2, 无 runtime 标志,
    #    与 Fcitx5 一致); 带 runtime 的 ad-hoc 在 macOS 26 上行为存疑. 这里对 ad-hoc 产物原地
    #    重签去掉 runtime 标志.
    #    注: ad-hoc 重签 (`-s -`) 不涉及 keychain/证书, 普通用户即可, 幂等.
    #    若 build 用了真实证书 (SIGN_IDENTITY / 已公证), 则用该证书重签, 但同样去 hardened-runtime.
    # 检测须用 --verbose=2: 默认 -dv (verbose=1) 不打印 "Signature=adhoc" 行 (踩过的坑).
    # 判据: CodeDirectory flags 里含 adhoc / 或 Signature=adhoc; 真证书则有 Authority=Developer ID.
    # SIGN_IDENTITY 非空: 用固定自签证书重签 (csreq 基于证书身份而非 cdhash, 重新部署 .app
    #   后辅助功能/TCC 授权不失效; 证书由 sign-setup 在本机创建)。去 hardened-runtime, 仅换签名身份。
    # 注意: 不要给 IME 加 --options runtime。旧 Go 仓实测「带 runtime 的 ad-hoc」反而异常;
    #   能稳定被 IMK 拉起控制器的是纯 ad-hoc/无 runtime (与 Fcitx5 一致)。多数「装上、列表里有、
    #   能切但无法输入」的根因不是签名, 而是 TIS 注册缓存污染 → 需注销重登做一次全量重扫。
    if [[ -n "${SIGN_IDENTITY:-}" ]]; then
        # 无头 ssh 部署: login keychain 在该 security session 默认锁定, codesign 访问私钥
        # 会 errSecInternalComponent。提供 SIGN_KEYCHAIN_PW 则先解锁 (本地 GUI 部署无需)。
        if [[ -n "${SIGN_KEYCHAIN_PW:-}" ]]; then
            security unlock-keychain -p "$SIGN_KEYCHAIN_PW" "$HOME/Library/Keychains/login.keychain-db" 2>/dev/null \
                && info "已解锁 login keychain (供 codesign)" || info "解锁 login keychain 失败"
        fi
        info "固定证书重签 .app: \"$SIGN_IDENTITY\" (去 hardened-runtime, 仅换签名身份)"
        # 失败不中止 (.app 已就位, 原签名多半仍可用), 但**必须让人看见**: 重签没成
        # 就等于留着 hardened-runtime 的签名, 症状正是「列表里有、能切、但打不出字」,
        # 而这种症状极易被误判成 TIS 缓存问题去反复注销重登。
        if ! codesign --force --sign "$SIGN_IDENTITY" --deep "$INSTALL_APP" 2>&1 | sed 's/^/    /'; then
            warn "重签失败! 装好的 .app 仍带原签名(可能含 hardened-runtime)。"
            warn "若切过去打不出字, 先手动重签再试, 而不是反复注销:"
            warn "  codesign --force --sign \"$SIGN_IDENTITY\" --deep \"$INSTALL_APP\""
        fi
    elif codesign -dv --verbose=2 "$INSTALL_APP" 2>&1 | grep -qi "adhoc"; then
        info "ad-hoc 产物: 去 hardened-runtime 重签 (codesign --force --sign -)"
        codesign --force --sign - --deep "$INSTALL_APP" 2>&1 | sed 's/^/    /' || true
    else
        info "(检测到真实证书签名, 保留原签名不重签)"
    fi
    codesign -dv --verbose=2 "$INSTALL_APP" 2>&1 | grep -E "Authority|flags|Signature" | sed 's/^/    /'

    # 4. 让系统重新发现 IME bundle.
    #    macOS 改 IME plist 后, 仅 cp 进 Input Methods/ 不足以让系统刷新 "输入源" 列表 ——
    #    LaunchServices 用 ChangeCount 缓存 bundle 信息, 不会因为 .app 替换而主动失效.
    #    必须显式跑 lsregister -f 强制重读, 才能让新字段 (ComponentInputModeDict 等) 进入索引.
    local LSREGISTER="/System/Library/Frameworks/CoreServices.framework/Frameworks/LaunchServices.framework/Support/lsregister"

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
    local APP_EXEC="$INSTALL_APP/Contents/MacOS/WindInput"
    local REGISTER_PID
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

    # .app 刚被整包替换, 但此刻可能还有一个**旧** bundle 的控制器进程在跑 (p1 的顺序是
    # 先装 service——那一步的 kick 会让 IMK 用旧 .app 重拉一个控制器——再装 app)。
    # 不踢掉它就留下「跑着旧二进制 / 连接已失效」的控制器, 表现正是
    # 「输入法能切过去, 但一个字都打不出来」, 且极易被误判成 TIS 缓存问题去反复注销重登。
    # 这里补一次 kick: 只杀控制器实例, 保留上面刚 fork 的 --register-input-source 守护。
    kick_ime_app

    bold "==> Done"
    cat <<EOF

  下一步:
    1. 打开 系统设置 → 键盘 → 文本输入 → 编辑 → 添加 (+) → 简体中文 → 选 $APP_NAME
       如果列表里看不到, 按下面顺序排查:
         a) ls -la "$INSTALL_APP" 看 .app 是否真的在
         b) /usr/libexec/PlistBuddy -c "Print" "$INSTALL_APP/Contents/Info.plist" | head -40
            必须有 InputMethodConnectionName / InputMethodServerControllerClass /
            ComponentInputModeDict / LSUIElement=true (不能是 LSBackgroundOnly);
            *不应* 出现 tsInputModeDefaultStateKey (有的话该 mode 会被「+」列表过滤掉)
         c) codesign -dv "$INSTALL_APP" 应输出 adhoc 签名信息 (flags 不含 runtime)
         d) 注销重登一次系统 (最暴力但有效, 让 TextInputSources 全量重扫)
    2. 切到 $APP_NAME (Ctrl+Space 或菜单栏 IME 切换)
    3. 在任意文本框敲一个字母键, 然后:
         log stream --predicate 'process == "WindInput"' --info --debug

  卸载:    scripts/mac/dev.sh $([[ "$APP_NAME" == WindInputDev ]] && echo ud || echo u)

EOF
}

# ───────────── service_install (装 Rust 服务 + LaunchAgent) ─────────────
# 把 Rust 服务 (wind_input + data/) 装到 per-user 目录, 以 LaunchAgent 形式注册为开机自启常驻进程。
#
# 服务定位词库用 exeDir/data (wind-config Config::data_dir = current_exe()/data),
# 所以二进制和 data/ 必须同目录 (本函数装到 INSTALL_ROOT 与 INSTALL_ROOT/data)。
# 用户数据 (config.toml / userdata.redb / socket) 走运行时目录
# (~/Library/Application Support/WindInput{Dev}), 与 service/ 子目录互不干扰。
#
# 以普通用户运行 (LaunchAgent 是 per-user gui domain, 不要 sudo)。
#
# 参数:
#   (无)            装 release 产物 (target/release)
#   --dev           装 dev 产物 (target/dev-variant); plist 注入 WIND_VARIANT=dev
#   --data <dir>    指定 data/ 源目录 (默认 SRC_DIR/data 或 build_mac/data)
#   --from <dir>    从指定目录装 (内含 wind_input + data), 供 .pkg postinstall 等场景
#   --uninstall     卸载服务 (保留用户数据)
#
# 变体身份 (务必对齐 Rust): dev 服务二进制用中文显示名 (文件名不带 _dev), 故 Rust 的
# variant::is_dev() 无法从 exe 文件名判定 → 必须靠 plist 里 WIND_VARIANT=dev 声明,
# Rust 才会用 WindInputDev 数据目录 + _dev 管道后缀, 与 dev .app (bundleID …Dev) 连通。
service_install() {
    local RUST_TARGET; RUST_TARGET="$(cargo_target_dir "$RUST_DIR")"
    local LOG_DIR="$HOME/Library/Logs"
    local GUI_DOMAIN="gui/$(id -u)"

    local DEV_VARIANT=0
    local DO_UNINSTALL=0
    local SRC_DIR=""
    local DATA_DIR=""
    local EXE_NAME="wind_input"
    while [[ $# -gt 0 ]]; do
        case "$1" in
            --dev)       DEV_VARIANT=1 ;;
            --uninstall) DO_UNINSTALL=1 ;;
            --data)      shift; DATA_DIR="${1:-}"; [[ -n "$DATA_DIR" ]] || { echo "[错误] --data 缺目录参数" >&2; exit 1; } ;;
            --from)      shift; SRC_DIR="${1:-}"; [[ -n "$SRC_DIR" ]] || { echo "[错误] --from 缺目录参数" >&2; exit 1; } ;;
            *) echo "[错误] 未知参数: $1" >&2; exit 1 ;;
        esac
        shift
    done

    # 变体派生: dev 用独立 LaunchAgent label + 运行时目录 (WindInputDev, 与 Rust
    # variant::app_dir_name() 对齐), 让 dev/release 两套服务共存, 各连各自的 .app socket。
    # 安装后可执行名装为中文名: macOS 后台列表 (BTM) 对无 Developer ID 的 legacy agent
    # 直接显示可执行文件名 (AssociatedBundleIdentifiers 被忽略)。二进制改名不影响功能。
    local LABEL APP_SUPPORT SVC_EXE_NAME ASSOC_BUNDLE LOG_TAG ENV_BLOCK
    ENV_BLOCK=""
    if [[ $DEV_VARIANT -eq 1 ]]; then
        LABEL="to.feng.windinput.service.dev"
        APP_SUPPORT="$HOME/Library/Application Support/WindInputDev"
        SVC_EXE_NAME="清风输入法服务开发版"
        ASSOC_BUNDLE="to.feng.inputmethod.WindInputDev"
        LOG_TAG="windinput_dev"
        # dev 服务二进制文件名不带 _dev, 靠此环境变量向 Rust 声明 dev 身份 (选 WindInputDev 数据目录)。
        ENV_BLOCK=$'    <key>EnvironmentVariables</key>\n    <dict>\n        <key>WIND_VARIANT</key>\n        <string>dev</string>\n    </dict>\n'
    else
        LABEL="to.feng.windinput.service"
        APP_SUPPORT="$HOME/Library/Application Support/WindInput"
        SVC_EXE_NAME="清风输入法服务"
        ASSOC_BUNDLE="to.feng.inputmethod.WindInput"
        LOG_TAG="windinput"
    fi
    local INSTALL_ROOT="$APP_SUPPORT/service"
    local INSTALL_EXE="$INSTALL_ROOT/$SVC_EXE_NAME"
    local PLIST="$HOME/Library/LaunchAgents/$LABEL.plist"
    local OUT_LOG="$LOG_DIR/$LOG_TAG.out.log"
    local ERR_LOG="$LOG_DIR/$LOG_TAG.err.log"
    local PUSH_SOCK="$APP_SUPPORT/bridge_push.sock"

    if [[ $EUID -eq 0 ]]; then
        err "请以普通用户运行 (LaunchAgent 是 per-user gui domain). 不要 sudo."
        exit 1
    fi

    # -------- uninstall --------
    if [[ $DO_UNINSTALL -eq 1 ]]; then
        bold "==> Uninstall Rust service ($LABEL)"
        if launchctl print "$GUI_DOMAIN/$LABEL" >/dev/null 2>&1; then
            launchctl bootout "$GUI_DOMAIN/$LABEL" 2>/dev/null || true
            info "bootout $GUI_DOMAIN/$LABEL"
        else
            info "(service 未加载)"
        fi
        [[ -f "$PLIST" ]] && { rm -f "$PLIST"; info "removed $PLIST"; } || info "(no $PLIST)"
        # 只删 service/ 子目录 (二进制+预制词库), 保留用户数据 (../config.toml, userdata.redb)。
        [[ -d "$INSTALL_ROOT" ]] && { rm -rf "$INSTALL_ROOT"; info "removed $INSTALL_ROOT"; } || info "(no $INSTALL_ROOT)"
        bold "==> Done (用户数据保留在 $APP_SUPPORT/)"
        exit 0
    fi

    # -------- 解析源目录 --------
    # WIND_MAC_UNIVERSAL=1 时 build_service 的产物落在 target/universal/<profile>/,
    # 原生路径下留着的是上一次单架构构建的**旧**二进制 —— 不认这个开关就会装错一个,
    # 且两者外观一致, 只有跑起来才发现不是刚编的那个。
    if [[ -z "$SRC_DIR" ]]; then
        local prof="release"; [[ $DEV_VARIANT -eq 1 ]] && prof="dev-variant"
        if [[ "${WIND_MAC_UNIVERSAL:-0}" == 1 ]]; then
            SRC_DIR="$RUST_TARGET/$UNIVERSAL_SUBDIR/$prof"
        else
            SRC_DIR="$RUST_TARGET/$prof"
        fi
    fi
    local SRC_EXE="$SRC_DIR/$EXE_NAME"
    # data 源: 优先 --data; 否则 SRC_DIR/data (与二进制同目录); 再否则 build_mac/data (gd 产物)。
    if [[ -z "$DATA_DIR" ]]; then
        if [[ -d "$SRC_DIR/data" ]]; then DATA_DIR="$SRC_DIR/data"; else DATA_DIR="$DATA_SNAPSHOT"; fi
    fi

    [[ -f "$SRC_EXE" ]]  || { err "未找到二进制 $SRC_EXE, 先跑 scripts/mac/dev.sh $([[ $DEV_VARIANT -eq 1 ]] && echo dm2 || echo m2)"; exit 1; }
    [[ -d "$DATA_DIR" ]] || { err "未找到词库目录 $DATA_DIR, 先跑 scripts/mac/dev.sh gd 组装 data"; exit 1; }

    # -------- install --------
    bold "==> Install Rust service -> $INSTALL_ROOT"

    # 1. 停旧服务实例。
    if launchctl print "$GUI_DOMAIN/$LABEL" >/dev/null 2>&1; then
        info "停止旧服务实例"
        launchctl bootout "$GUI_DOMAIN/$LABEL" 2>/dev/null || true
    fi
    # 清理孤儿进程 (前台跑过或上次 bootout 漏网的旧 wind_input 会占着 socket)。
    # 按 service 目录精确匹配, 不误杀同名其它进程。
    if pgrep -f "$INSTALL_ROOT/" >/dev/null 2>&1; then
        info "清理残留的旧服务进程"
        pkill -f "$INSTALL_ROOT/" 2>/dev/null || true
        sleep 1
    fi
    rm -f "$INSTALL_ROOT/wind_input"  # 删旧文件名残留 (升级到中文名时)

    # 2. 复制二进制 + 词库 (data/ 用 rsync --delete 与源一致)。
    mkdir -p "$INSTALL_ROOT" "$LOG_DIR" "$HOME/Library/LaunchAgents"
    cp -f "$SRC_EXE" "$INSTALL_EXE"
    chmod +x "$INSTALL_EXE"
    # 原地重签: 跨机/同路径部署时内核 amfi 缓存上版 cdhash, 新二进制经 launchd 起来会校验失配。
    # --force 重签刷新; ad-hoc 幂等。SIGN_IDENTITY 设则用固定证书 (无头 ssh 需 SIGN_KEYCHAIN_PW 解锁)。
    if command -v codesign >/dev/null; then
        if [[ -n "${SIGN_IDENTITY:-}" ]]; then
            if [[ -n "${SIGN_KEYCHAIN_PW:-}" ]]; then
                security unlock-keychain -p "$SIGN_KEYCHAIN_PW" "$HOME/Library/Keychains/login.keychain-db" 2>/dev/null || true
            fi
            codesign --force -s "$SIGN_IDENTITY" "$INSTALL_EXE" 2>/dev/null \
                && info "固定证书重签服务二进制: \"$SIGN_IDENTITY\"" \
                || info "codesign 重签跳过 (非致命)"
        else
            codesign --force -s - "$INSTALL_EXE" 2>/dev/null \
                && info "ad-hoc 重签服务二进制" \
                || info "codesign 重签跳过 (非致命)"
        fi
    fi
    if command -v rsync >/dev/null; then
        rsync -a --delete "$DATA_DIR/" "$INSTALL_ROOT/data/"
    else
        rm -rf "$INSTALL_ROOT/data"; cp -R "$DATA_DIR" "$INSTALL_ROOT/data"
    fi
    info "已复制 服务二进制 + data/ ($(find "$INSTALL_ROOT/data" -type f | wc -l | tr -d ' ') 个数据文件)"

    # 3. 写 LaunchAgent plist (RunAtLoad 开机自启 + KeepAlive 崩溃自拉起; dev 注入 WIND_VARIANT=dev)。
    cat > "$PLIST" <<PLIST_EOF
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>Label</key>
    <string>$LABEL</string>
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
${ENV_BLOCK}    <key>StandardOutPath</key>
    <string>$OUT_LOG</string>
    <key>StandardErrorPath</key>
    <string>$ERR_LOG</string>
</dict>
</plist>
PLIST_EOF
    info "已写 $PLIST$([[ $DEV_VARIANT -eq 1 ]] && echo ' (含 WIND_VARIANT=dev)')"

    # 4. 加载 + 启用 + 启动。
    launchctl bootstrap "$GUI_DOMAIN" "$PLIST" 2>/dev/null || {
        err "bootstrap 失败, 重试一次 (可能旧实例未完全退出)"
        launchctl bootout "$GUI_DOMAIN/$LABEL" 2>/dev/null || true
        launchctl bootstrap "$GUI_DOMAIN" "$PLIST"
    }
    launchctl enable "$GUI_DOMAIN/$LABEL" 2>/dev/null || true
    launchctl kickstart -k "$GUI_DOMAIN/$LABEL" 2>/dev/null || true
    info "bootstrap + enable + kickstart 完成"

    # 5. 验证 (等服务起 socket)。
    bold "==> Verify"
    local i
    for i in 1 2 3 4 5 6 7 8 9 10; do
        [[ -S "$PUSH_SOCK" ]] && break
        sleep 0.3
    done
    local STATE PID
    STATE=$(launchctl print "$GUI_DOMAIN/$LABEL" 2>/dev/null | grep -E '^[[:space:]]*state =' | head -1 | sed 's/^[[:space:]]*//' || true)
    PID=$(launchctl print "$GUI_DOMAIN/$LABEL" 2>/dev/null | grep -E '^[[:space:]]*pid =' | head -1 | sed 's/^[[:space:]]*//' || true)
    info "launchd: ${STATE:-未知} ${PID:-}"
    if [[ -S "$PUSH_SOCK" ]]; then
        info "✓ push socket 存在: $PUSH_SOCK"
    else
        err "✗ push socket 未出现: $PUSH_SOCK (看 $ERR_LOG)"
    fi
    if [[ -s "$ERR_LOG" ]]; then
        info "err.log 尾部:"; tail -5 "$ERR_LOG" | sed 's/^/    /'
    else
        info "✓ err.log 为空"
    fi

    # 6. 服务已(重)起, 踢一下正在运行的本变体 IME app 让其重连 (否则旧连接失效=按键无响应)。
    kick_ime_app

    bold "==> Done"
    cat <<EOF

  服务已注册为开机自启 ($LABEL).
  状态: launchctl print $GUI_DOMAIN/$LABEL | grep -E 'state|pid'
  重启: launchctl kickstart -k $GUI_DOMAIN/$LABEL
  日志: $OUT_LOG / $ERR_LOG
  卸载: scripts/mac/dev.sh $([[ $DEV_VARIANT -eq 1 ]] && echo ud || echo u)
EOF
}

# ───────────── 组合: 编 + 装 ─────────────
install_service() {
    # 显式判构建结果: errexit 在本脚本失效 (见 run_tokens 处说明), 不判就会「构建失败
    # 却把上一次的旧产物装进系统」——装完看不出异常, 只是行为还是老版本。
    build_service || { err "service 构建失败, 不予安装"; return 1; }
    local data; data="$(resolve_data)"
    bold "==> 安装 service ($VARIANT, data=$data)"
    service_install ${APP_VARIANT_FLAG[@]+"${APP_VARIANT_FLAG[@]}"} --data "$data"
}

install_app() {
    # 同 install_service: 必须显式判。此处漏判尤其坏——下方会把 build/ 里的 .app 删掉,
    # 于是「构建失败 → 装了旧 app → 证据也被清掉」, 事后完全查不出装进去的是哪一版。
    build_app || { err ".app 构建失败, 不予安装"; return 1; }
    bold "==> 安装 app ($VARIANT)"
    app_install ${APP_VARIANT_FLAG[@]+"${APP_VARIANT_FLAG[@]}"}
    # 防复发: 删掉 build/ 里的 .app 并注销其 LS 登记。它与 ~/Library 里的真身同 bundle-ID,
    # 留着会被 LaunchServices 自动登记成「重复输入源」, TIS 可能错指向它(尤其路径后被删=
    # 幽灵)→ 控制器拉不起 → 无法输入。真身已装在 ~/Library, build/ 仅中间产物, 可删。
    local appname; appname="$(app_name_for_variant)"
    local built="$MACOS_DIR/build/$appname.app"
    if [[ -d "$built" ]]; then
        local lsreg="/System/Library/Frameworks/CoreServices.framework/Frameworks/LaunchServices.framework/Support/lsregister"
        [[ -x "$lsreg" ]] && "$lsreg" -u "$built" 2>/dev/null || true
        rm -rf "$built"
        info "已清理 build/ 重复 .app(防 LS 重复登记导致无法输入)"
    fi
    check_accessibility_grant
}

# 辅助功能授权是否仍然有效(cdhash 比对)。
#
# 我们用自签证书(无 Team ID / 非 Apple 锚定), TCC 只能把授权钉死在**当次构建的 cdhash**
# 上——授权记录里的 csreq 就是一条裸 `cdhash H"…"`(对照: 正规签名的 app 存的是
# `anchor apple generic` + Team ID, 与具体构建无关)。于是每次重新部署 cdhash 一变,
# 那条授权就再也匹配不上: **系统设置里的开关还亮着, 实际已失效**。
#
# 受影响的是命令直通车的按键合成与智能配对的宿主光标回退——它们会**静默不工作**。
# 故装完主动比对一次, 不一致就明说该怎么办。
#
# 读 TCC.db 需要终端有「完全磁盘访问」; 读不到就跳过(不报错, 更不阻断安装)。
check_accessibility_grant() {
    # bundleID 从已装 .app 的 plist 直接读，避免在此再复制一份变体派生规则。
    local appname; appname="$(app_name_for_variant)"
    local app_path="$HOME/Library/Input Methods/$appname.app"
    [[ -d "$app_path" ]] || return 0
    local bid
    bid="$(/usr/libexec/PlistBuddy -c "Print :CFBundleIdentifier" \
        "$app_path/Contents/Info.plist" 2>/dev/null)" || return 0
    [[ -n "$bid" ]] || return 0
    command -v sqlite3 >/dev/null || return 0

    local cur; cur="$(codesign -dvvv "$app_path" 2>&1 | sed -n 's/^CDHash=//p')"
    [[ -n "$cur" ]] || return 0
    local granted
    granted="$(sqlite3 "/Library/Application Support/com.apple.TCC/TCC.db" \
        "select lower(hex(csreq)) from access where service='kTCCServiceAccessibility' and client='$bid' and auth_value=2" \
        2>/dev/null)" || return 0
    [[ -n "$granted" ]] || return 0   # 从未授权过: 首次用到时系统会自己弹框, 不必在此提示

    # csreq 尾部 20 字节即被钉住的 cdhash(裸 `cdhash H"…"` 形态)。非该形态则跳过判断。
    local pinned="${granted: -40}"
    if [[ "$pinned" == "$cur" ]]; then
        info "辅助功能授权有效 (cdhash 匹配)"
        return 0
    fi
    warn "辅助功能授权已失效: 系统设置里开关仍是开的, 但它被钉在旧构建的 cdhash 上"
    warn "  已授权: $pinned"
    warn "  本  次: $cur"
    warn "  影响: 命令直通车按键合成 / 智能配对的光标回退会静默不工作"
    warn "  临时解法: tccutil reset Accessibility $bid  然后触发一次相关功能, 重新授权"
    warn "  根治: 改用带 Team ID 的证书签名(如 Apple Development), TCC 便按身份而非 cdhash 记授权"
}

install_all() {
    install_service || { err "service 安装失败, 中止"; return 1; }
    install_app     || { err ".app 安装失败, 中止"; return 1; }
    install_setting || warn "设置程序未安装 (非致命: 仓库缺失或构建失败; IME 本身可用)"
    bold "==> 系统安装完成 ($VARIANT) — 切到 $(app_name_for_variant) 试输入"
}

do_full() {
    bold "========== 全构建 ($VARIANT) =========="
    # 每步显式判退出码: 本脚本 errexit 失效 (见文件末 run_tokens 处说明), 漏判就会
    # 「某步失败了却一路跑到『全构建完成』」——正是 codesign 失败仍报成功的老形态。
    build_service || { err "service 构建失败, 中止全构建"; return 1; }
    build_app     || { err ".app 构建失败, 中止全构建"; return 1; }
    # wind_setting 是独立仓库, 允许缺席; 但失败与否要如实反映在下方产物清单里。
    local setting_ok=1
    build_setting || { setting_ok=0; warn "wind_setting 构建跳过/失败 (非致命, 设置 app 缺失)"; }
    do_gendata        || { err "gen-data 失败, 中止全构建"; return 1; }
    verify_dist_data  || { err "发布数据校验失败, 中止全构建"; return 1; }

    bold "========== 全构建完成 ($VARIANT) =========="
    local setting_disp="清风输入法设置"; [[ $VARIANT == dev ]] && setting_disp="清风输入法设置开发版"
    local prof; prof=$([[ $VARIANT == dev ]] && echo dev-variant || echo release)
    info "产物:"
    info "  service = target/$prof/wind_input"
    info "  app     = $MACOS_DIR/build/$(app_name_for_variant).app"
    # 只报真的在那儿的东西 —— 此前无条件打印设置 app 路径, 构建失败时也照报, 等于骗人。
    if (( setting_ok )) && [[ -d "$MACOS_DIR/build/$setting_disp.app" ]]; then
        info "  setting = $MACOS_DIR/build/$setting_disp.app"
    else
        warn "  setting = (缺失: wind_setting 未构建成功; 装出来的系统里没有设置程序)"
    fi
    info "  data    = $DATA_SNAPSHOT"
    info "下一步: scripts/mac/dev.sh $([[ $VARIANT == dev ]] && echo pd1 || echo p1)  (系统安装)"
}

do_run() {
    bold "==> 重启 service ($LABEL)"
    launchctl kickstart -k "gui/$(id -u)/$LABEL" && info "kickstart 完成" || err "kickstart 失败 (service 未安装?)"
    # 服务已重启, 踢一下 IME app 让其重连新服务 (否则按键无响应)。
    kick_ime_app
}

do_logs() {
    local tag="windinput"; [[ "$VARIANT" == dev ]] && tag="windinput_dev"
    local pn; pn="$(app_name_for_variant)"
    bold "==> 跟踪日志 ($VARIANT, Ctrl-C 退出)"
    info "service: ~/Library/Logs/$tag.out.log | IME: log stream process==$pn"
    # 同时跟 service 文件日志 + IME 系统日志 (renderFrame/forwarder)。
    ( log stream --predicate "process == \"$pn\"" --info 2>/dev/null | grep --line-buffered -E 'renderFrame|forwarder|bridge|handle|caret' & )
    tail -F "$HOME/Library/Logs/$tag.out.log"
}

do_status() {
    bold "==> service 状态 ($VARIANT)"
    launchctl print "gui/$(id -u)/$LABEL" 2>/dev/null | grep -E 'state =|pid =' | head || warn "service 未注册"
    info "二进制: $(ls -la "$APP_SUPPORT/service/"*服务* 2>/dev/null | awk '{print $6,$7,$8}' | head -1)"
    info "push socket: $([[ -S "$APP_SUPPORT/bridge_push.sock" ]] && echo 存在 || echo 缺失)"
    local app="$HOME/Library/Input Methods/$(app_name_for_variant).app"
    info ".app 签名: $(codesign -dv --verbose=2 "$app" 2>&1 | grep -E 'flags=' | sed 's/.*flags/flags/' || echo 未装)"
    info "IME 进程: $(pgrep -fl "Input Methods/$(app_name_for_variant)" | head -1 || echo 未运行)"
}

do_data() {
    [[ -d "$INSTALLED_DATA" ]] || { err "当前未装 service data ($INSTALLED_DATA), 无法快照"; exit 1; }
    rm -rf "$DATA_SNAPSHOT"; mkdir -p "$DATA_SNAPSHOT"
    cp -R "$INSTALLED_DATA/." "$DATA_SNAPSHOT/"
    info "已快照 data/ → build_mac/data ($(find "$DATA_SNAPSHOT" -type f | wc -l | tr -d ' ') 文件)"
}

do_uninstall() {
    bold "==> 卸载 service + app + 设置 ($VARIANT)"
    # 内联函数在卸载分支用 `exit 0` 终止; 放进子 shell 以免终止整个 dev.sh (此处需顺序卸多端)。
    ( service_install ${APP_VARIANT_FLAG[@]+"${APP_VARIANT_FLAG[@]}"} --uninstall ) || true
    ( app_install     ${APP_VARIANT_FLAG[@]+"${APP_VARIANT_FLAG[@]}"} --uninstall ) || true
    ( setting_install ${APP_VARIANT_FLAG[@]+"${APP_VARIANT_FLAG[@]}"} --uninstall ) || true
}

# 候选 REPL (本机)。
do_repl() {
    local data="${1:-}"
    [[ -z "$data" ]] && data="$DATA_SNAPSHOT"
    [[ -d "$data" ]] || warn "词库数据不存在 ($data); 先跑 gd 生成"
    bold "==> 启动候选 REPL (data=$data)"
    ( cd "$RUST_DIR" && WIND_DATA="$data" cargo run --release -p wind-repl -- "$data" )
}

# ───────────── sign_setup (命令行建自签证书) ─────────────
# 用 openssl + security cli, 完全跳过 Keychain Access GUI. 输出可用于 codesign 的本机证书 "WindInput Dev".
# 用法: scripts/mac/dev.sh sign-setup [create|check|grant|remove]
sign_setup() {
    # 原逻辑用 `set -uo pipefail` (无 errexit): 多处依赖命令失败继续 (find/delete 探测、清理循环)。
    # 这里在函数内关掉 errexit 以原样保留其控制流。
    set +e

    local CERT_NAME="WindInput Dev"
    local WORK_DIR="${TMPDIR:-/tmp}/wind_input_cert"
    local CFG_FILE="$WORK_DIR/openssl.cnf"
    local KEY_FILE="$WORK_DIR/cert.key"
    local CRT_FILE="$WORK_DIR/cert.crt"
    local P12_FILE="$WORK_DIR/cert.p12"
    local P12_PASS="windinput-dev"

    # purge_cert — 删除所有同名证书, 带次数上限防死循环。login + System (sudo) 都试, 20 次封顶。
    purge_cert() {
        local i=0
        while security find-certificate -c "$CERT_NAME" >/dev/null 2>&1; do
            security delete-certificate -c "$CERT_NAME" >/dev/null 2>&1
            sudo security delete-certificate -c "$CERT_NAME" /Library/Keychains/System.keychain >/dev/null 2>&1
            i=$((i + 1))
            if [[ $i -ge 20 ]]; then
                err "清理 \"$CERT_NAME\" 超过 20 次仍残留, 放弃 (手动检查 login/System keychain)"
                break
            fi
        done
    }

    local SUB="${1:-create}"

    # ---------------- check ----------------
    if [[ "$SUB" == "check" ]]; then
        bold "查询当前 codesigning identity"
        security find-identity -v -p codesigning
        exit 0
    fi

    # ---------------- grant ----------------
    # 授权 codesign 非交互访问私钥 (set-key-partition-list)。无头 ssh 部署才能用证书签名。
    if [[ "$SUB" == "grant" ]]; then
        local KEYCHAIN="$HOME/Library/Keychains/login.keychain-db"
        bold "授权 codesign 非交互访问 \"$CERT_NAME\" 私钥"
        printf "  输入此 Mac 的登录密码 (解锁 login keychain): "
        local PW; read -rs PW; echo
        if ! security unlock-keychain -p "$PW" "$KEYCHAIN"; then
            err "解锁 login keychain 失败 (密码错?)"; unset PW; exit 1
        fi
        if security set-key-partition-list -S apple-tool:,apple:,codesign: -s -k "$PW" "$KEYCHAIN" >/dev/null 2>&1; then
            bold "成功: codesign 现可非交互访问私钥 (ssh 无头部署可用此证书签名)"
        else
            err "set-key-partition-list 失败"
        fi
        unset PW
        exit 0
    fi

    # ---------------- remove ----------------
    if [[ "$SUB" == "remove" ]]; then
        bold "删 \"$CERT_NAME\" 证书 (所有同名条目, 含 System keychain)"
        purge_cert
        sudo security remove-trusted-cert -d -p codeSign 2>/dev/null || true
        bold "remove 完成"
        exit 0
    fi

    # ---------------- create ----------------
    command -v openssl  >/dev/null || { err "openssl 未安装"; exit 1; }
    command -v security >/dev/null || { err "security cli 未安装"; exit 1; }

    # 清理已有同名证书 (失败的 import 也会留条目, 重复后 codesign ambiguous)。
    if security find-certificate -c "$CERT_NAME" >/dev/null 2>&1; then
        bold "发现已有 \"$CERT_NAME\" 证书, 清掉重建"
        purge_cert
    fi

    mkdir -p "$WORK_DIR"
    chmod 700 "$WORK_DIR"

    bold "1. 生成 openssl 配置 (X509 extensions for code signing)"
    cat > "$CFG_FILE" <<EOF
[ req ]
distinguished_name = req_distinguished_name
prompt             = no
x509_extensions    = v3_self

[ req_distinguished_name ]
CN = $CERT_NAME
O  = WindInput Local
C  = CN

[ v3_self ]
basicConstraints       = critical, CA:false
keyUsage               = critical, digitalSignature
extendedKeyUsage       = critical, codeSigning
subjectKeyIdentifier   = hash
EOF
    info "$CFG_FILE"

    bold "2. 生成 RSA 2048 私钥 + 自签 X509 证书 (有效期 10 年)"
    openssl req -x509 -newkey rsa:2048 -nodes \
        -keyout "$KEY_FILE" -out "$CRT_FILE" \
        -days 3650 -config "$CFG_FILE" -sha256 2>&1 | tail -3 | sed 's/^/  /'
    [[ -f "$CRT_FILE" ]] || { err "openssl 生成失败"; exit 1; }

    bold "3. 打成 PKCS12 (.p12, legacy 格式) 以便 security import"
    # OpenSSL 3.x 默认 PBES2 macOS security import 不识别, 必须 -legacy 回退老格式;
    # 但 macOS 自带 LibreSSL 不认识 -legacy (且默认就是老格式) → 仅对 OpenSSL 3.x 加 -legacy。
    local P12_LEGACY=""
    if openssl version 2>/dev/null | grep -qi "^OpenSSL 3"; then
        P12_LEGACY="-legacy"
    fi
    openssl pkcs12 -export $P12_LEGACY -inkey "$KEY_FILE" -in "$CRT_FILE" \
        -out "$P12_FILE" -name "$CERT_NAME" -passout pass:"$P12_PASS" 2>&1 | tail -3 | sed 's/^/  /'
    [[ -f "$P12_FILE" ]] || { err "pkcs12 生成失败 (openssl 版本/参数不兼容?), 终止"; exit 1; }

    bold "4a. unlock login keychain (会弹一次密码框)"
    local KEYCHAIN="$HOME/Library/Keychains/login.keychain-db"
    security unlock-keychain "$KEYCHAIN" || {
        err "解锁失败. 请手动跑: security unlock-keychain ~/Library/Keychains/login.keychain-db"
        exit 1
    }

    bold "4b. import 到 login keychain (允许 codesign 直接用)"
    # -T /usr/bin/codesign: 把 codesign 加入私钥 ACL; -A: 允许所有应用使用此私钥 (开发期方便)。
    security import "$P12_FILE" -k "$KEYCHAIN" \
        -P "$P12_PASS" -A 2>&1 | sed 's/^/  /'

    bold "5. 把证书加为 trusted code-signing root (这一步要 sudo)"
    # 没有 trust, codesign 用上后系统仍判 CSSMERR_TP_NOT_TRUSTED 等同 ad-hoc, IME 注册照样拒。
    sudo security add-trusted-cert -d -r trustRoot -p codeSign \
        -k "/Library/Keychains/System.keychain" "$CRT_FILE" 2>&1 | sed 's/^/  /'

    bold "6. 验证 identity 可用 (Valid identities only 段应出现 \"$CERT_NAME\")"
    security find-identity -v -p codesigning | sed 's/^/  /'

    if security find-identity -v -p codesigning | grep -q "\"$CERT_NAME\""; then
        bold "成功"
        info "现在跑:"
        info "  SIGN_IDENTITY=\"$CERT_NAME\" scripts/mac/dev.sh 1     # 全构建 (会用此证书签 app)"
        info "  SIGN_IDENTITY=\"$CERT_NAME\" scripts/mac/dev.sh p1    # 系统安装"
    else
        err "证书仍未 valid. 看上面 add-trusted-cert 输出"
        exit 1
    fi

    rm -rf "$WORK_DIR"
}

# ───────────── pkg_build (打 .pkg 安装器) ─────────────
# 把 IME (.app) + Rust 服务 (wind_input + data) [+ 可选 设置 app] 打成单个 .pkg 安装器。
# postinstall 复用 payload 内 install_*.sh 薄包装 (re-exec dev.sh __app_install / __service_install)。
#
# 产物: wind_macos/dist/WindInput[Dev]-<版本>-macOS.pkg
#
# 用法:
#   pkg_build [release|dev]            # 用现有构建产物打包 (缺 .app 自动转构建)
#   pkg_build [release|dev] --build    # 先构建 (cargo + gen-data + app_build) 再打包
#
# 通用二进制: WIND_MAC_UNIVERSAL=1 → 三个二进制都按 arm64+x86_64 构建, hostArchitectures
# 一并放开, 且在 pkgbuild 前逐个 lipo -archs 校验 (缺架构即拒绝出包)。
#
# 公证 (预留): 配齐则 productbuild 后自动 productsign + notarytool + staple:
#   MACOS_DEVELOPER_ID_INSTALLER / MACOS_NOTARY_APPLE_ID / MACOS_NOTARY_PASSWORD / MACOS_NOTARY_TEAM_ID
pkg_build() {
    local PROFILE="release"
    case "${1:-}" in release|dev) PROFILE="$1"; shift ;; esac
    apply_variant "$PROFILE"

    local DEPLOY_DIR="$SCRIPT_DIR"   # 安装脚本/postinstall 资源与本脚本同目录 (scripts/mac)
    local SUFFIX=""; [[ "$PROFILE" == dev ]] && SUFFIX="Dev"
    local APP_NAME="WindInput$SUFFIX"
    local APP_BUNDLE="$MACOS_DIR/build/$APP_NAME.app"
    local SERVICE_BIN; SERVICE_BIN="$(service_bin_path)"                    # 随 universal 开关变
    local SERVICE_DATA="$DATA_SNAPSHOT"                                     # gd 产物 (变体无关)
    # 设置 app: build_setting 组装的壳 (变体名不同); 可选 (设置仓缺失则跳过)。
    local SETTING_DISP="清风输入法设置"; [[ "$PROFILE" == dev ]] && SETTING_DISP="清风输入法设置开发版"
    local SETTING_EXE="wind_setting"; [[ "$PROFILE" == dev ]] && SETTING_EXE="wind_setting_dev"
    local SETTING_APP="$MACOS_DIR/build/$SETTING_DISP.app"

    local DIST_DIR="$MACOS_DIR/dist"
    local PKG_ID="to.feng.windinput.installer$([[ "$PROFILE" == dev ]] && echo .dev)"
    # postinstall 硬编码 STAGE=…/WindInputInstaller, 故两变体共用同一暂存名 (装机时一次一个, 装完即清)。
    local STAGE_REL="Library/Application Support/WindInputInstaller"
    # dev 变体: 生成的 install_*.sh 包装脚本注入 --dev, 让 service/app 装成 dev 身份。
    local WRAP_VFLAG=""; [[ "$PROFILE" == dev ]] && WRAP_VFLAG=" --dev"

    local DO_BUILD=0
    local arg
    for arg in "$@"; do
        case "$arg" in
            --build) DO_BUILD=1 ;;
            *) echo "[错误] 未知参数: $arg" >&2; exit 1 ;;
        esac
    done

    # install/app 装完会删 build/*.app（防 LaunchServices 重复登记），故 pkg 多半找不到现成 .app。
    # 缺 .app 时自动转构建模式（发行包本就应全新构建），免去手动 --build。
    if [[ $DO_BUILD -eq 0 && ! -d "$APP_BUNDLE" ]]; then
        info "未找到 build/$APP_NAME.app → 自动构建 (等同 --build)"
        DO_BUILD=1
    fi

    command -v pkgbuild >/dev/null || { err "pkgbuild 未找到 (macOS 自带 Xcode CLT)"; exit 1; }

    # -------- (可选) 构建 --------
    if [[ $DO_BUILD -eq 1 ]]; then
        bold "==> 构建 IME + 服务 + 词库 + 设置 ($PROFILE$([[ $UNIVERSAL -eq 1 ]] && echo ', universal'))"
        # 走 build_service 而非内联 cargo —— 内联会绕过 universal 分支, 打出个"名义通用"的包。
        build_service || { err "服务构建失败, 中止打包"; exit 1; }
        do_gendata                             # 组装 data/ → build_mac/data
        app_build ${APP_VARIANT_FLAG[@]+"${APP_VARIANT_FLAG[@]}"}   # IME .app
        build_setting || warn "wind_setting 构建跳过/失败 (非致命, .pkg 将不含设置 app)"   # 设置 .app (可选)
    fi

    # -------- 校验必备产物 (设置 app 可选) --------
    local miss=0 p
    for p in "$APP_BUNDLE" "$SERVICE_BIN" "$SERVICE_DATA"; do
        [[ -e "$p" ]] || { err "缺产物: $p"; miss=1; }
    done
    [[ $miss -eq 0 ]] || { err "请先跑 scripts/mac/dev.sh $([[ "$PROFILE" == dev ]] && echo d8 || echo 8) (或手动构建各组件)"; exit 1; }
    local HAVE_SETTING=0
    [[ -e "$SETTING_APP" ]] && HAVE_SETTING=1 || info "(无 wind_setting.app, 跳过设置组件)"

    local VERSION
    VERSION=$(/usr/libexec/PlistBuddy -c "Print CFBundleShortVersionString" "$APP_BUNDLE/Contents/Info.plist" 2>/dev/null || echo "0.0.0")
    local PKG_PATH="$DIST_DIR/$APP_NAME-${VERSION}-macOS.pkg"

    # -------- 组 payload root --------
    bold "==> 组装 payload ($PROFILE, 版本 $VERSION)"
    # 不用 local：EXIT trap 在 pkg_build 返回后、脚本退出时才触发，若是 local 则那时已出作用域
    # → set -u 报 "PKGROOT: unbound variable"（产物其实已生成，仅清理时报错）。设为脚本级全局。
    PKGROOT=$(mktemp -d)
    SCRIPTS=$(mktemp -d)
    trap 'rm -rf "${PKGROOT:-}" "${SCRIPTS:-}" 2>/dev/null || true' EXIT

    local DEST="$PKGROOT/$STAGE_REL"
    mkdir -p "$DEST/service"
    cp -R "$APP_BUNDLE"   "$DEST/"
    cp    "$SERVICE_BIN"  "$DEST/service/wind_input"
    cp -R "$SERVICE_DATA" "$DEST/service/data"
    # 安装脚本: 把 dev.sh 本身放进 payload, 再生成两个薄包装脚本 (postinstall 仍按
    # install_app.sh / install_service.sh 这两个名字调用)。包装脚本 re-exec dev.sh 的内部入口
    # __app_install / __service_install, 绕过面向交互的 flag 解析, 保留原 install_* 行为。
    # dev 变体在包装里注入 --dev (让服务/IME 装成 dev 身份, plist 含 WIND_VARIANT=dev)。
    # SIGN_IDENTITY 默认设空 (终端用户机无自签证书 → 走 ad-hoc; 打包方显式设了则照常继承)。
    cp "$DEPLOY_DIR/dev.sh" "$DEST/dev.sh"
    cat > "$DEST/install_service.sh" <<WRAP
#!/bin/bash
export SIGN_IDENTITY="\${SIGN_IDENTITY-}"
exec "\$(cd "\$(dirname "\$0")" && pwd)/dev.sh" __service_install${WRAP_VFLAG} "\$@"
WRAP
    cat > "$DEST/install_app.sh" <<WRAP
#!/bin/bash
export SIGN_IDENTITY="\${SIGN_IDENTITY-}"
exec "\$(cd "\$(dirname "\$0")" && pwd)/dev.sh" __app_install${WRAP_VFLAG} "\$@"
WRAP
    if [[ $HAVE_SETTING -eq 1 ]]; then
        cp -R "$SETTING_APP" "$DEST/"
        # 生成设置安装薄包装 (postinstall 按 install_setting.sh --from <STAGE> 调用):
        # re-exec dev.sh __setting_install, 从 STAGE 找同名 <设置>.app 装到用户 ~/Applications。
        cat > "$DEST/install_setting.sh" <<WRAP
#!/bin/bash
export SIGN_IDENTITY="\${SIGN_IDENTITY-}"
exec "\$(cd "\$(dirname "\$0")" && pwd)/dev.sh" __setting_install${WRAP_VFLAG} "\$@"
WRAP
    fi
    chmod +x "$DEST"/*.sh "$DEST/service/wind_input"
    info "payload: $APP_NAME.app + service(wind_input+data)$([[ $HAVE_SETTING -eq 1 ]] && echo ' + wind_setting.app') + 安装脚本"

    # -------- universal 硬校验 --------
    # 要了通用二进制却少一个架构 = "名义通用、实则单架构"。这种包在 Intel Mac 上**装得上**
    # (pkg 只是拷文件), 只在真正拉起进程时才失败, 且失败面是"输入法装了但没反应"这类难查的
    # 表现。宁可在这里出不了包, 也不能把它发出去。
    if [[ $UNIVERSAL -eq 1 ]]; then
        local to_check=("$DEST/service/wind_input" "$DEST/$APP_NAME.app/Contents/MacOS/WindInput")
        [[ $HAVE_SETTING -eq 1 ]] && to_check+=("$DEST/$SETTING_DISP.app/Contents/MacOS/$SETTING_EXE")
        local b archs
        for b in "${to_check[@]}"; do
            archs="$(lipo -archs "$b" 2>/dev/null || echo '<读不出>')"
            if [[ "$archs" != *arm64* || "$archs" != *x86_64* ]]; then
                err "架构不全 [$archs]: $b"
                err "已指定通用二进制 (WIND_MAC_UNIVERSAL=1) 却缺架构, 拒绝出包。"
                exit 1
            fi
            info "  ✓ [$archs] $(basename "$b")"
        done
    fi

    # -------- postinstall --------
    cp "$SCRIPT_DIR/pkg_resources/postinstall" "$SCRIPTS/postinstall"
    chmod +x "$SCRIPTS/postinstall"

    # -------- component plist: 关掉 BundleIsRelocatable --------
    local COMP="$SCRIPTS/components.plist"
    pkgbuild --analyze --root "$PKGROOT" "$COMP" >/dev/null
    /usr/bin/python3 - "$COMP" <<'PY'
import plistlib, sys
p = sys.argv[1]
with open(p, "rb") as f:
    arr = plistlib.load(f)
for c in arr:
    c["BundleIsRelocatable"] = False
with open(p, "wb") as f:
    plistlib.dump(arr, f)
PY
    info "已关闭 BundleIsRelocatable (锁定到暂存路径)"

    # -------- pkgbuild + productbuild --------
    bold "==> pkgbuild + productbuild"
    mkdir -p "$DIST_DIR"
    rm -f "$PKG_PATH"
    # hostArchitectures 必须与 payload 的真实架构一致: 写宽了 (声明 x86_64 但二进制没有)
    # 会让 Intel Mac 装完发现跑不起来; 写窄了则 Intel Mac 上安装器直接拒装。
    local HOST_ARCHS="arm64"; [[ $UNIVERSAL -eq 1 ]] && HOST_ARCHS="arm64,x86_64"
    info "hostArchitectures: $HOST_ARCHS"

    local COMPONENT_PKG="$SCRIPTS/$APP_NAME-component.pkg"
    pkgbuild \
        --root "$PKGROOT" \
        --component-plist "$COMP" \
        --scripts "$SCRIPTS" \
        --identifier "$PKG_ID" \
        --version "$VERSION" \
        --install-location "/" \
        "$COMPONENT_PKG"

    local TITLE="清风输入法$([[ "$PROFILE" == dev ]] && echo '开发版') $VERSION"
    local DIST_XML="$SCRIPTS/distribution.xml"
    cat > "$DIST_XML" <<XML
<?xml version="1.0" encoding="utf-8"?>
<installer-gui-script minSpecVersion="2">
    <title>$TITLE</title>
    <options customize="never" require-scripts="true" hostArchitectures="$HOST_ARCHS"/>
    <domains enable_anywhere="false" enable_currentUserHome="false" enable_localSystem="true"/>
    <choices-outline><line choice="default"/></choices-outline>
    <choice id="default" title="$TITLE"><pkg-ref id="$PKG_ID"/></choice>
    <pkg-ref id="$PKG_ID" version="$VERSION" onConclusion="none">$(basename "$COMPONENT_PKG")</pkg-ref>
</installer-gui-script>
XML

    productbuild \
        --distribution "$DIST_XML" \
        --package-path "$SCRIPTS" \
        "$PKG_PATH"

    # -------- (预留) Developer ID 签名 + 公证 --------
    local NOTARIZED=0
    if [[ -n "${MACOS_DEVELOPER_ID_INSTALLER:-}" ]]; then
        bold "==> productsign (Developer ID Installer)"
        local SIGNED_PKG="${PKG_PATH%.pkg}-signed.pkg"
        productsign --sign "$MACOS_DEVELOPER_ID_INSTALLER" "$PKG_PATH" "$SIGNED_PKG"
        mv -f "$SIGNED_PKG" "$PKG_PATH"
        info "已签名: $PKG_PATH"
        if [[ -n "${MACOS_NOTARY_APPLE_ID:-}" && -n "${MACOS_NOTARY_PASSWORD:-}" && -n "${MACOS_NOTARY_TEAM_ID:-}" ]]; then
            bold "==> notarytool submit --wait + stapler staple"
            xcrun notarytool submit "$PKG_PATH" \
                --apple-id "$MACOS_NOTARY_APPLE_ID" --password "$MACOS_NOTARY_PASSWORD" \
                --team-id "$MACOS_NOTARY_TEAM_ID" --wait
            xcrun stapler staple "$PKG_PATH"
            NOTARIZED=1
            info "已公证 + staple: $PKG_PATH"
        else
            info "(已签名但未配 notarytool 凭据, 跳过公证)"
        fi
    else
        info "(未配 MACOS_DEVELOPER_ID_INSTALLER, 保持 ad-hoc 产物)"
    fi

    bold "==> Done"
    info "PKG: $PKG_PATH ($(du -h "$PKG_PATH" | cut -f1))"
    info "安装: sudo installer -pkg \"$PKG_PATH\" -target /   (或双击走向导)"
    if [[ $NOTARIZED -eq 0 ]]; then
        info "(未公证版首启需 右键→打开 绕过 Gatekeeper; Tahoe 系统设置 UI 硬墙需公证才解)"
    fi
    if [[ "$PROFILE" == dev ]]; then
        warn "注: dev .pkg 的 payload/安装脚本已是 dev 身份, 但 postinstall 生成的桌面卸载器仍按 release 清理;"
        warn "    卸载 dev 请用 scripts/mac/dev.sh ud。"
    fi
}

# ───────────────────────── 菜单 ─────────────────────────
show_menu() {
    printf "\033[36m============================================\033[0m\n"
    printf "\033[36m  WindInput 开发菜单 (macOS 原生)\033[0m\n"
    printf "\033[36m============================================\033[0m\n"
    printf "\033[33m  全构建 (service + app + gen-data + 校验):\033[0m\n"
    echo   "    1    Release 全构建            d1   Dev 全构建"
    printf "\033[33m  单模块构建 (前缀 d = dev):\033[0m\n"
    echo   "    m1   仅 app  (WindInput.app)    dm1"
    echo   "    m2   仅 service (wind_service)  dm2"
    echo   "    m3   仅 设置 app (wind_setting) dm3"
    printf "\033[33m  系统安装 / 卸载:\033[0m\n"
    echo   "    p1   安装全部 (release)         pd1   安装全部 (dev)"
    echo   "    pm1/pm2/pm3  装模块(app/svc/设置)  pdm1/pdm2/pdm3 (dev)"
    echo   "    u/u1 卸载全部 (release)         ud/ud1 卸载全部 (dev)"
    printf "\033[33m  安装包 (.pkg):\033[0m\n"
    echo   "    8    生成安装包 (release)        d8    生成安装包 (dev)"
    echo   "    8s   跳过重建直接打包 (release)  d8s   跳过重建直接打包 (dev)"
    printf "\033[33m  代码质量:\033[0m\n"
    echo   "    k=check  l=clippy  t=test  f=fmt  fmt-check  ci  hooks  clean"
    printf "\033[33m  数据 / 实测:\033[0m\n"
    echo   "    gd=gen-data  r=repl(本机)"
    printf "\033[33m  macOS 便利命令:\033[0m\n"
    echo   "    run  logs  status  data  sign-setup  pkg"
    echo   "    q=退出"
    printf "\033[36m============================================\033[0m\n"
}

# ───────────── 统一分发 (菜单与命令行直调共用) ─────────────
# 返回 127 = 未知命令 (区别于命令执行失败)。
dispatch() {
    case "$1" in
        1|release) apply_variant release; do_full ;;
        d1|dev)    apply_variant dev;     do_full ;;
        m1)   apply_variant release; build_app ;;
        dm1)  apply_variant dev;     build_app ;;
        m2)   apply_variant release; build_service ;;
        dm2)  apply_variant dev;     build_service ;;
        m3)   apply_variant release; build_setting ;;
        dm3)  apply_variant dev;     build_setting ;;
        p1)   apply_variant release; install_all ;;
        pd1)  apply_variant dev;     install_all ;;
        pm1)  apply_variant release; install_app ;;
        pdm1) apply_variant dev;     install_app ;;
        pm2)  apply_variant release; install_service ;;
        pdm2) apply_variant dev;     install_service ;;
        pm3)  apply_variant release; install_setting ;;
        pdm3) apply_variant dev;     install_setting ;;
        u|u1)   apply_variant release; do_uninstall ;;
        ud|ud1) apply_variant dev;     do_uninstall ;;
        8)    pkg_build release --build ;;
        8s)   pkg_build release ;;
        d8)   pkg_build dev --build ;;
        d8s)  pkg_build dev ;;
        k|check)   do_cargo check ;;
        l|clippy)  do_cargo clippy ;;
        t|test)    do_cargo test ;;
        f|fmt)     ( cd "$RUST_DIR" && bold "==> cargo fmt" && cargo fmt ) ;;
        fmt-check) ( cd "$RUST_DIR" && bold "==> cargo fmt --check" && cargo fmt --all -- --check ) ;;
        ci)        do_ci ;;
        hooks)     do_hooks_install ;;
        clean)     do_cargo clean ;;
        gd|gen-data) apply_variant release; do_gendata && verify_dist_data ;;
        run)       apply_variant release; do_run ;;
        logs|log)  apply_variant release; do_logs ;;
        status|st) apply_variant release; do_status ;;
        data)      apply_variant release; do_data ;;
        *) return 127 ;;
    esac
}

do_cargo() { bold "==> cargo $1 --workspace"; ( cd "$RUST_DIR" && cargo "$1" --workspace ); }

# 激活仓库自带 pre-commit hook (提交前跑 cargo fmt --check)。对齐 dev.ps1 Do-HooksInstall。
# 纯本地 git config, 不随仓库传播 —— 每个 clone/worktree 都要单独激活一次。
do_hooks_install() {
    bold "==> 激活 .githooks/pre-commit (git config core.hooksPath .githooks)"
    ( cd "$REPO_DIR" && git config core.hooksPath .githooks ) || return $?
    chmod +x "$REPO_DIR/.githooks/pre-commit" 2>/dev/null || true
    info "已激活：提交前将自动跑 cargo fmt --check"
}

do_ci() {
    ( cd "$RUST_DIR" && bold "==> cargo fmt --check" && cargo fmt --all -- --check ) || return $?
    do_cargo clippy || return $?
    do_cargo test   || return $?
    bold "==> CI 全部通过 ✓"
}

# 顺序执行一串命令 token (空格分隔); 前者失败即停。每个命令在子 shell 里跑, 保留内联函数的
# `exit` 语义又不终止整个脚本。
#
# ⚠️⚠️ 本脚本里 `set -e` (errexit) 是**失效**的, 别指望它 ⚠️⚠️
#
# 下面 `( dispatch "$cmd" ) || rc=$?` 把子 shell 放在了 `||` 列表左侧。按 POSIX/bash
# 语义, 处于 `&&`/`||` 列表中(非末项)的命令一律豁免 errexit, 且该豁免会**穿透**进子 shell、
# 函数体与其中所有嵌套调用 —— bash 手册原话是「即使设置了 -e 也不生效」。实测连在子 shell
# 里重新 `set -e` 都救不回来。
#
# 后果: dispatch 之下(build_service / app_build / do_gendata / …)任何命令失败都**不会**
# 自动中止, 脚本会若无其事地跑到「完成」。历史事故: codesign 失败后原样留下 ad-hoc 签名,
# 却照报构建成功, 装上去表现为「能切输入法但打不出字」。
#
# 因此: 关键步骤必须**显式**判退出码 (`cmd || return 1` / `if ! cmd; then … fi`),
# 尤其是签名、安装、数据校验这类失败后果不可见的步骤。新增步骤时照此办理。
run_tokens() {
    local toks=("$@") n=$# i=0 cmd rc
    while (( i < n )); do
        cmd="${toks[$i]}"
        case "$cmd" in
            r|repl)
                local d=""
                if (( i + 1 < n )); then d="${toks[$((i + 1))]}"; ((i++)); fi
                rc=0; ( do_repl "$d" ) || rc=$?
                (( rc != 0 )) && { err "命令 '$cmd' 失败 (退出码 $rc)"; return "$rc"; }
                ;;
            sign-setup)
                local sub=""
                if (( i + 1 < n )); then sub="${toks[$((i + 1))]}"; ((i++)); fi
                rc=0; ( sign_setup ${sub:+"$sub"} ) || rc=$?
                (( rc != 0 )) && { err "命令 '$cmd' 失败 (退出码 $rc)"; return "$rc"; }
                ;;
            pkg)
                local pargs=()
                while (( i + 1 < n )); do ((i++)); pargs+=("${toks[$i]}"); done
                rc=0; ( pkg_build release ${pargs[@]+"${pargs[@]}"} ) || rc=$?
                (( rc != 0 )) && { err "命令 '$cmd' 失败 (退出码 $rc)"; return "$rc"; }
                ;;
            *)
                rc=0; ( dispatch "$cmd" ) || rc=$?
                if (( rc == 127 )); then err "未知命令: $cmd (试 'scripts/mac/dev.sh help')"; return 1; fi
                (( rc != 0 )) && { err "命令 '$cmd' 失败 (退出码 $rc)"; return "$rc"; }
                ;;
        esac
        ((i++))
    done
    return 0
}

menu_loop() {
    local raw toks
    while true; do
        show_menu
        printf "\n请输入选项 (可空格分隔连续命令): "
        read -r raw || return 0
        raw="$(printf '%s' "$raw" | tr -d '\r')"
        [[ -z "$raw" ]] && continue
        case "$raw" in q|Q) return 0 ;; esac
        read -ra toks <<< "$raw"
        [[ "${toks[0]}" == menu ]] && continue
        run_tokens "${toks[@]}" || true
        printf "\n按回车继续..."; read -r _ || true
    done
}

print_help() {
    # 打印顶部 # 注释块 (对齐 dev.ps1 的 --help)。排除 shebang (#!)。
    grep -E '^#( |$)' "${BASH_SOURCE[0]}" | sed -E 's/^# ?//'
}

# ───────────────────────── 入口 ─────────────────────────
# .pkg postinstall 经 payload 内 install_app.sh / install_service.sh 薄包装 re-exec 本脚本时
# 走这里: 原样把参数 (--from/--data/--uninstall/--dev) 交给内联函数, 绕过下方 token 分发。
case "${1:-}" in
    __service_install) shift; service_install "$@"; exit $? ;;
    __app_install)     shift; app_install "$@"; exit $? ;;
    __setting_install) shift; setting_install "$@"; exit $? ;;
esac

# 解析全局 --data, 其余收进 token 列表 (命令 + 子参数)。
TOKENS=()
while [[ $# -gt 0 ]]; do
    case "$1" in
        --data) shift; DATA_OVERRIDE="${1:-}"; [[ -n "$DATA_OVERRIDE" ]] || { err "--data 缺目录"; exit 1; } ;;
        *) TOKENS+=("$1") ;;
    esac
    shift
done

# 无参数 → 交互菜单
if [[ ${#TOKENS[@]} -eq 0 ]]; then menu_loop; exit 0; fi

case "${TOKENS[0]}" in
    -h|--help|help) print_help; exit 0 ;;
    menu)           menu_loop; exit 0 ;;
esac

rc=0
run_tokens "${TOKENS[@]}" || rc=$?
exit "$rc"
