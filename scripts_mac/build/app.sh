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
set -euo pipefail

SCRIPT_DIR=$(cd "$(dirname "$0")" && pwd)
REPO_DIR=$(cd "$SCRIPT_DIR/../.." && pwd)
MACOS_DIR="$REPO_DIR/wind_macos"
APP_NAME="WindInput"
APP_BUNDLE="$MACOS_DIR/build/$APP_NAME.app"

SWIFT_CONFIG="release"
DO_SIGN=1
# 默认 ad-hoc (-). 真实证书:
#   SIGN_IDENTITY="WindInput Dev" scripts_mac/build/app.sh
# 自签证书的创建方法见 scripts_mac/deploy/setup_signing.md.
# macOS 26 (Tahoe) 对 IME 强制要求 codesign 有真实 Authority, adhoc 被 TIS
# 静默拒绝注册 — 本地开发期请用自签证书签名.
SIGN_IDENTITY="${SIGN_IDENTITY:-}"
for arg in "$@"; do
    case "$arg" in
        --debug)   SWIFT_CONFIG="debug" ;;
        --no-sign) DO_SIGN=0 ;;
        *) echo "[错误] 未知参数: $arg" >&2; exit 1 ;;
    esac
done

bold() { printf "\033[1m%s\033[0m\n" "$*"; }
info() { printf "  %s\n" "$*"; }
err()  { printf "\033[31m[错误] %s\033[0m\n" "$*" >&2; }

command -v swift    >/dev/null || { err "swift 未安装 (装 Xcode CLT)"; exit 1; }
command -v codesign >/dev/null || { err "codesign 未安装 (装 Xcode CLT)"; exit 1; }

bold "==> Build wind-input-app ($SWIFT_CONFIG)"
cd "$MACOS_DIR"
swift build -c "$SWIFT_CONFIG" --product wind-input-app

# SwiftPM 把二进制放在 .build/<config>/wind-input-app
BIN_PATH="$MACOS_DIR/.build/$SWIFT_CONFIG/wind-input-app"
[[ -x "$BIN_PATH" ]] || { err "二进制未找到: $BIN_PATH"; exit 1; }
info "binary: $BIN_PATH ($(stat -f%z "$BIN_PATH") bytes)"

bold "==> Assemble $APP_BUNDLE"
rm -rf "$APP_BUNDLE"
mkdir -p "$APP_BUNDLE/Contents/MacOS" "$APP_BUNDLE/Contents/Resources"

# 二进制 → Contents/MacOS/WindInput (与 Info.plist 的 CFBundleExecutable 对齐)
cp "$BIN_PATH" "$APP_BUNDLE/Contents/MacOS/$APP_NAME"
chmod +x "$APP_BUNDLE/Contents/MacOS/$APP_NAME"

# Info.plist
cp "$MACOS_DIR/Sources/WindInputApp/Resources/Info.plist" "$APP_BUNDLE/Contents/Info.plist"

# 本地化字符串 (输入法菜单名 / 应用显示名).
# Resources/{zh-Hans,en}.lproj/InfoPlist.strings → Contents/Resources/<lang>.lproj/InfoPlist.strings
for lproj in "$MACOS_DIR/Sources/WindInputApp/Resources"/*.lproj; do
    [[ -d "$lproj" ]] || continue
    lang=$(basename "$lproj")
    mkdir -p "$APP_BUNDLE/Contents/Resources/$lang"
    cp -R "$lproj"/* "$APP_BUNDLE/Contents/Resources/$lang/"
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
