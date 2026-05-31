#!/usr/bin/env bash
# build_macos_app.sh — 拼装 WindInput.app bundle (PR-A M2).
#
# SwiftPM 不直接产 .app, 这里:
#   1. swift build --product wind-input-app  (release, arm64)
#   2. 按标准 macOS .app 结构拼 Contents/{MacOS, Resources, Info.plist}
#   3. codesign --force --sign - (ad-hoc 签名, 让本机能加载; 上架走 PR-A.5 M6)
#
# 输出: wind_macos/build/WindInput.app
#
# 用法:
#   scripts_mac/build/app.sh            # release build + ad-hoc 签名
#   scripts_mac/build/app.sh --debug    # debug build (swift build -c debug)
#   scripts_mac/build/app.sh --no-sign  # 不 codesign (调试用)
#   scripts_mac/build/app.sh --universal # arm64+x86_64 通用二进制 (分发/CI 用)
#   WIND_MAC_UNIVERSAL=1 scripts_mac/build/app.sh  # 同上 (CI 走环境变量统一开关)
set -euo pipefail

SCRIPT_DIR=$(cd "$(dirname "$0")" && pwd)
REPO_DIR=$(cd "$SCRIPT_DIR/../.." && pwd)
MACOS_DIR="$REPO_DIR/wind_macos"
# 变体: release → APP_NAME=WindInput; debug → APP_NAME=WindInputDebug (--debug 设置)。
# .app 目录名/bundleID 按变体区分以支持共存; 可执行名 EXE_NAME 恒为 WindInput
# (= CFBundleExecutable, 两变体同名, 仅所在 .app 路径不同)。
APP_BASE="WindInput"
VARIANT_SUFFIX=""        # debug 时 "Debug"
EXE_NAME="$APP_BASE"

SWIFT_CONFIG="release"
DO_SIGN=1
# universal: arm64+x86_64 通用二进制. 环境变量 WIND_MAC_UNIVERSAL=1 或 --universal 开启.
# 默认本机单架构 (本地/VM 快). CI 在 job 级设环境变量, 三件套脚本统一继承同一开关.
UNIVERSAL="${WIND_MAC_UNIVERSAL:-0}"
# 默认 ad-hoc (-). 真实证书:
#   SIGN_IDENTITY="WindInput Dev" scripts_mac/build/app.sh
# 自签证书的创建方法见 scripts_mac/deploy/setup_signing.md.
# macOS 26 (Tahoe) 对 IME 强制要求 codesign 有真实 Authority, adhoc 被 TIS
# 静默拒绝注册 — 本地开发期请用自签证书签名.
SIGN_IDENTITY="${SIGN_IDENTITY:-}"
for arg in "$@"; do
    case "$arg" in
        --debug)     SWIFT_CONFIG="debug"; VARIANT_SUFFIX="Debug" ;;
        --no-sign)   DO_SIGN=0 ;;
        --universal) UNIVERSAL=1 ;;
        *) echo "[错误] 未知参数: $arg" >&2; exit 1 ;;
    esac
done

# 变体派生: APP_NAME = .app 目录名 + bundleID 后缀 (WindInput / WindInputDebug)。
APP_NAME="${APP_BASE}${VARIANT_SUFFIX}"
APP_BUNDLE="$MACOS_DIR/build/$APP_NAME.app"

bold() { printf "\033[1m%s\033[0m\n" "$*"; }
info() { printf "  %s\n" "$*"; }
err()  { printf "\033[31m[错误] %s\033[0m\n" "$*" >&2; }

command -v swift    >/dev/null || { err "swift 未安装 (装 Xcode CLT)"; exit 1; }
command -v codesign >/dev/null || { err "codesign 未安装 (装 Xcode CLT)"; exit 1; }

bold "==> Build wind-input-app ($SWIFT_CONFIG$([[ $UNIVERSAL -eq 1 ]] && echo ", universal"))"
cd "$MACOS_DIR"
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

# 变体注入 (debug): 全局把 bundleID 串换成 debug 变体 —— 一并改写 CFBundleIdentifier /
# InputMethodConnectionName / ComponentInputModeDict 的 mode-id (作 dict key + TISInputSourceID
# 值 + 有序数组项)。再把显示名 (CFBundleName/DisplayName/TISIconLabels) 加「开发版」。
# 这样 debug .app 注册为独立输入源, 与 release 共存。
if [[ -n "$VARIANT_SUFFIX" ]]; then
    bold "==> 变体注入 (debug): bundleID/mode/连接名/显示名 → $APP_NAME"
    sed -i '' \
        -e 's/to\.feng\.inputmethod\.WindInput/to.feng.inputmethod.WindInputDebug/g' \
        -e 's/清风输入法/清风输入法开发版/g' \
        "$APP_BUNDLE/Contents/Info.plist"
fi

# 版本贯通: 从仓库根 VERSION 文件 (CI 由 tag 写入) 注入 CFBundleShortVersionString /
# CFBundleVersion. pkg.sh 后续读 CFBundleShortVersionString 作 .pkg 文件名/版本/向导标题,
# 故版本真源是 VERSION 文件. 无 VERSION 文件时保持 plist 原值 (0.0.0), 不破坏纯本地构建.
VERSION_FILE="$REPO_DIR/VERSION"
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
for lproj in "$MACOS_DIR/Sources/WindInputApp/Resources"/*.lproj; do
    [[ -d "$lproj" ]] || continue
    lang=$(basename "$lproj")
    mkdir -p "$APP_BUNDLE/Contents/Resources/$lang"
    cp -R "$lproj"/* "$APP_BUNDLE/Contents/Resources/$lang/"
    # 变体注入 (debug): mode-id 键对齐 + 本地化显示名加「开发版」/「Debug」。
    if [[ -n "$VARIANT_SUFFIX" && -f "$APP_BUNDLE/Contents/Resources/$lang/InfoPlist.strings" ]]; then
        sed -i '' \
            -e 's/to\.feng\.inputmethod\.WindInput/to.feng.inputmethod.WindInputDebug/g' \
            -e 's/"清风输入法"/"清风输入法开发版"/g' \
            -e 's/"WindInput"/"WindInputDebug"/g' \
            "$APP_BUNDLE/Contents/Resources/$lang/InfoPlist.strings"
    fi
    info "lproj: $lang"
done

# 菜单栏图标 (单色 PDF 模板). plist 引用 menu_icon.pdf, 另带 _15 / _26 应对 Retina.
# 源 SVG 在 Resources/wind-{15,26}.svg, 重新生成: rsvg-convert -f pdf -o menu_icon_15.pdf wind-15.svg
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
APPICON="$MACOS_DIR/Sources/WindInputApp/Resources/AppIcon.icns"
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
    ENTS="$MACOS_DIR/Sources/WindInputApp/Resources/WindInput.entitlements"
    if [[ -n "$SIGN_IDENTITY" ]]; then
        bold "==> codesign with identity \"$SIGN_IDENTITY\" + hardened runtime"
        SIGN_ARGS=(--force --sign "$SIGN_IDENTITY" --options runtime --timestamp=none)
    else
        bold "==> codesign ad-hoc + hardened runtime (macOS 26 上 TIS 会拒绝, 请用 SIGN_IDENTITY)"
        SIGN_ARGS=(--force --sign - --options runtime --timestamp=none)
    fi
    if [[ -f "$ENTS" ]]; then
        SIGN_ARGS+=(--entitlements "$ENTS")
    fi
    codesign "${SIGN_ARGS[@]}" "$APP_BUNDLE"
    codesign -dv --verbose=2 "$APP_BUNDLE" 2>&1 | sed 's/^/    /' | head -12
fi

bold "==> Done"
info "Bundle: $APP_BUNDLE"
info "下一步: sudo scripts_mac/deploy/install_app.sh"
info "       (会把 .app 复制到 /Library/Input Methods/ 并 killall 旧实例)"
