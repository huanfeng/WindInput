#!/usr/bin/env bash
# pkg.sh — 把三件套 (IME + Go 服务 + 设置 app) 打成单个 .pkg 安装器 (面向终端用户分发).
#
# 为何 .pkg 而非 DMG 自装: 多组件 (输入法 + 后台服务 + LaunchAgent + 设置 app) 安装到
# 多个 per-user 目录, .pkg 的 payload + postinstall 是标准方案; postinstall 复用
# deploy/install_{app,service,setting}.sh 的成熟逻辑 (不在 Swift 里重写一遍).
#
# 产物: wind_macos/dist/WindInput-<版本>.pkg
#
# 安装 (终端用户): 双击 .pkg 走 Installer 向导 (未签名首启需右键→打开);
#   或命令行 `sudo installer -pkg WindInput-<版本>.pkg -target /`.
# postinstall 会以登录 GUI 用户身份装三件套, 并在 ~/Applications 放「卸载清风输入法.command」.
#
# 注意 (未公证版): .pkg 未签名 → Gatekeeper 首启拦截需绕过; 且 macOS 26 Tahoe 对非公证
# IME 有系统设置 UI 硬墙 (见 wind_macos/AGENTS.md), 真正可分发需 Developer ID + 公证.
#
# 用法:
#   scripts_mac/build/pkg.sh             # 用现有构建产物打包
#   scripts_mac/build/pkg.sh --build     # 先构建 IME + 设置 + 服务/词库 再打包
#   scripts_mac/build/pkg.sh --universal # universal (arm64+x86_64) 产物 + 装包器
#   WIND_MAC_UNIVERSAL=1 scripts_mac/build/pkg.sh --build  # 同上 (CI 走环境变量统一开关)
#
# 公证 (预留, 可选): 配齐以下环境变量则在 productbuild 后自动 productsign + notarytool + staple,
# 否则保持 ad-hoc 产物不变 (无需改脚本即可将来启用):
#   MACOS_DEVELOPER_ID_INSTALLER  "Developer ID Installer: Name (TEAMID)" (productsign 身份)
#   MACOS_NOTARY_APPLE_ID / MACOS_NOTARY_PASSWORD / MACOS_NOTARY_TEAM_ID  (notarytool 凭据)
set -euo pipefail

SCRIPT_DIR=$(cd "$(dirname "$0")" && pwd)
REPO_DIR=$(cd "$SCRIPT_DIR/../.." && pwd)
MACOS_DIR="$REPO_DIR/wind_macos"
DEPLOY_DIR="$REPO_DIR/scripts_mac/deploy"

APP_BUNDLE="$MACOS_DIR/build/WindInput.app"
SETTING_APP="$REPO_DIR/wind_setting/build/bin/wind_setting.app"
SERVICE_BIN="$REPO_DIR/build/wind_input"
SERVICE_DATA="$REPO_DIR/build/data"

DIST_DIR="$MACOS_DIR/dist"
PKG_ID="to.feng.windinput.installer"
STAGE_REL="Library/Application Support/WindInputInstaller"   # payload 在系统中的落点

DO_BUILD=0
# universal 开关: 既驱动子构建脚本 (经 export 继承), 也决定 distribution.xml 的 hostArchitectures.
# --universal 时 export 给 build.sh/app.sh/setting.sh, 保证构建产物与装包器架构声明一致.
UNIVERSAL="${WIND_MAC_UNIVERSAL:-0}"
for arg in "$@"; do
    case "$arg" in
        --build) DO_BUILD=1 ;;
        --universal) UNIVERSAL=1; export WIND_MAC_UNIVERSAL=1 ;;
        *) echo "[错误] 未知参数: $arg" >&2; exit 1 ;;
    esac
done

bold() { printf "\033[1m%s\033[0m\n" "$*"; }
info() { printf "  %s\n" "$*"; }
err()  { printf "\033[31m[错误] %s\033[0m\n" "$*" >&2; }

command -v pkgbuild >/dev/null || { err "pkgbuild 未找到 (macOS 自带 Xcode CLT)"; exit 1; }

# -------- (可选) 构建 --------
if [[ $DO_BUILD -eq 1 ]]; then
    bold "==> 构建三件套"
    "$REPO_DIR/scripts_mac/build/build.sh" data        # 词库 (服务依赖)
    "$REPO_DIR/scripts_mac/build/build.sh" service     # Go 服务
    "$REPO_DIR/scripts_mac/build/app.sh"               # IME .app
    "$REPO_DIR/scripts_mac/build/setting.sh"           # 设置 app
fi

# -------- 校验产物 --------
miss=0
for p in "$APP_BUNDLE" "$SETTING_APP" "$SERVICE_BIN" "$SERVICE_DATA"; do
    [[ -e "$p" ]] || { err "缺产物: $p"; miss=1; }
done
[[ $miss -eq 0 ]] || { err "请先跑 scripts_mac/build/pkg.sh --build (或手动构建各组件)"; exit 1; }

VERSION=$(/usr/libexec/PlistBuddy -c "Print CFBundleShortVersionString" "$APP_BUNDLE/Contents/Info.plist" 2>/dev/null || echo "0.0.0")
# 文件名带 -macOS 后缀, 与 Windows 的 -Setup.exe / -Portable.zip 在同一 Release 里区分.
PKG_PATH="$DIST_DIR/WindInput-${VERSION}-macOS.pkg"

# -------- 组 payload root --------
bold "==> 组装 payload (版本 $VERSION)"
PKGROOT=$(mktemp -d)
SCRIPTS=$(mktemp -d)
trap 'rm -rf "$PKGROOT" "$SCRIPTS"' EXIT

DEST="$PKGROOT/$STAGE_REL"
mkdir -p "$DEST/service"
cp -R "$APP_BUNDLE"   "$DEST/"
cp -R "$SETTING_APP"  "$DEST/"
cp    "$SERVICE_BIN"  "$DEST/service/wind_input"
cp -R "$SERVICE_DATA" "$DEST/service/data"
cp "$DEPLOY_DIR/install_app.sh" "$DEPLOY_DIR/install_service.sh" "$DEPLOY_DIR/install_setting.sh" "$DEST/"
chmod +x "$DEST"/*.sh "$DEST/service/wind_input"
info "payload: WindInput.app + wind_setting.app + service(wind_input+data) + 3 安装脚本"

# -------- postinstall --------
cp "$SCRIPT_DIR/pkg_resources/postinstall" "$SCRIPTS/postinstall"
chmod +x "$SCRIPTS/postinstall"

# -------- component plist: 关掉 BundleIsRelocatable --------
# pkgbuild 默认把 .app 当可重定位 bundle, Installer 若在别处找到同 bundleID 的已有安装
# (如用户先前装过 wind_setting.app), 会把 payload 重定向过去而非铺到暂存目录, 导致
# postinstall 在 $STAGE 找不到产物. 强制 BundleIsRelocatable=false 锁死到暂存路径.
COMP="$SCRIPTS/components.plist"
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

# -------- pkgbuild (component) + productbuild (distribution) --------
# 用 productbuild distribution 才能自定义向导标题 (<title>) 并去掉"选择安装位置"步.
#
# 关键 (踩坑): 不带 hostArchitectures 的 distribution 会让 installer 跑内置架构自动检测,
# 在 tart 等虚拟化 macOS 上 cpuFeatures=null 直接崩 ("不能安装/需要 Rosetta", 即便产物
# 全是 arm64). **显式声明 hostArchitectures="arm64" 即跳过该检测**, VM 与真机都能装.
bold "==> pkgbuild + productbuild"
mkdir -p "$DIST_DIR"
rm -f "$PKG_PATH"

# hostArchitectures 必须与实际产物架构一致: universal 声明双架构, 否则单 arm64.
# 不一致会导致装包器在错误架构机器上拒装或装出跑不起来的二进制.
if [[ $UNIVERSAL -eq 1 ]]; then
    HOST_ARCHS="arm64,x86_64"
else
    HOST_ARCHS="arm64"
fi
info "hostArchitectures: $HOST_ARCHS"

COMPONENT_PKG="$SCRIPTS/WindInput-component.pkg"
pkgbuild \
    --root "$PKGROOT" \
    --component-plist "$COMP" \
    --scripts "$SCRIPTS" \
    --identifier "$PKG_ID" \
    --version "$VERSION" \
    --install-location "/" \
    "$COMPONENT_PKG"

DIST_XML="$SCRIPTS/distribution.xml"
cat > "$DIST_XML" <<XML
<?xml version="1.0" encoding="utf-8"?>
<installer-gui-script minSpecVersion="2">
    <title>清风输入法 $VERSION</title>
    <!-- hostArchitectures 必需: 不声明则 installer 内置架构检测在 VM 上崩 (见上).
         值随 universal 开关: arm64 或 arm64,x86_64, 与实际产物一致. -->
    <options customize="never" require-scripts="true" hostArchitectures="$HOST_ARCHS"/>
    <!-- 单一系统域 + customize=never: 去掉"选择安装位置/目标磁盘"步与自定义按钮. -->
    <domains enable_anywhere="false" enable_currentUserHome="false" enable_localSystem="true"/>
    <choices-outline><line choice="default"/></choices-outline>
    <choice id="default" title="清风输入法"><pkg-ref id="$PKG_ID"/></choice>
    <pkg-ref id="$PKG_ID" version="$VERSION" onConclusion="none">$(basename "$COMPONENT_PKG")</pkg-ref>
</installer-gui-script>
XML

productbuild \
    --distribution "$DIST_XML" \
    --package-path "$SCRIPTS" \
    "$PKG_PATH"

# -------- (预留) Developer ID 签名 + 公证 --------
# 凭据齐全才执行, 否则保持 ad-hoc 产物不变. 将来买了证书只需配环境变量, 无需改脚本.
NOTARIZED=0
if [[ -n "${MACOS_DEVELOPER_ID_INSTALLER:-}" ]]; then
    bold "==> productsign (Developer ID Installer)"
    SIGNED_PKG="${PKG_PATH%.pkg}-signed.pkg"
    productsign --sign "$MACOS_DEVELOPER_ID_INSTALLER" "$PKG_PATH" "$SIGNED_PKG"
    mv -f "$SIGNED_PKG" "$PKG_PATH"
    info "已签名: $PKG_PATH"

    if [[ -n "${MACOS_NOTARY_APPLE_ID:-}" && -n "${MACOS_NOTARY_PASSWORD:-}" && -n "${MACOS_NOTARY_TEAM_ID:-}" ]]; then
        bold "==> notarytool submit --wait + stapler staple"
        xcrun notarytool submit "$PKG_PATH" \
            --apple-id "$MACOS_NOTARY_APPLE_ID" \
            --password "$MACOS_NOTARY_PASSWORD" \
            --team-id "$MACOS_NOTARY_TEAM_ID" \
            --wait
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
info "卸载: 双击 ~/Applications/卸载清风输入法.app"
if [[ $NOTARIZED -eq 0 ]]; then
    info "(未公证版首启需 右键→打开 绕过 Gatekeeper; Tahoe 系统设置 UI 硬墙需公证才解)"
fi
