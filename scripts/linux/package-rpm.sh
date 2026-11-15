#!/usr/bin/env bash
# 本机打 .rpm（Fedora / openSUSE 通用）：把 package-deb.sh 留下的安装树交给 rpm 系容器里的 rpmbuild，
# 再用 verify-rpm.sh + smoke-rpm.sh 核对。二进制本身仍在 Ubuntu 22.04 基线（glibc 最低）上编，
# 所以先要跑过 package-deb.sh（同一个 WORK_DIR），这里只做 rpm 元数据与门禁，很快。
#
#   scripts/linux/package-deb.sh && scripts/linux/package-rpm.sh   # 产物 dist/linux/windinput-<版本>-1.<arch>.rpm
#
# 环境变量：
#   WORK_DIR     与 package-deb.sh 相同（默认 ~/.cache/wi-deb），从其中的 stage/pkg 取安装树
#   OUT_DIR      产物目录（默认 <仓库>/dist/linux）
#   RPM_IMAGES   用来冒烟的发行版镜像，空格分隔（默认 Fedora 与 openSUSE Leap；Docker Hub 不通时走 quay.io / registry.opensuse.org）
#   BUILD_IMAGE  rpmbuild 用的镜像（默认取 RPM_IMAGES 的第一个）
set -euo pipefail

REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
VERSION="$(tr -d '[:space:]' <"$REPO/docs/VERSION")"
WORK_DIR="${WORK_DIR:-$HOME/.cache/wi-deb}"
OUT_DIR="${OUT_DIR:-$REPO/dist/linux}"
RPM_IMAGES="${RPM_IMAGES:-quay.io/fedora/fedora:41 registry.opensuse.org/opensuse/leap:15.6}"
BUILD_IMAGE="${BUILD_IMAGE:-${RPM_IMAGES%% *}}"
STAGE="$WORK_DIR/stage/pkg"

[[ -d "$STAGE/usr/lib/windinput" ]] || { echo "没有安装树 $STAGE：先跑 scripts/linux/package-deb.sh（同一个 WORK_DIR）" >&2; exit 1; }
command -v docker >/dev/null || { echo "需要 docker" >&2; exit 1; }
mkdir -p "$OUT_DIR"
RPM_OUT="$(mktemp -d)"; trap 'rm -rf "$RPM_OUT"' EXIT

echo "[rpm] 打包 → $OUT_DIR（rpmbuild 在 $BUILD_IMAGE 里）"
docker run --rm -v "$REPO:/src:ro" -v "$STAGE:/stage:ro" -v "$RPM_OUT:/out" \
    -e APP_VERSION="$VERSION" -e STAGE_DIR=/stage -e SRC_DIR=/src -e OUT_DIR=/out \
    "$BUILD_IMAGE" bash -c 'command -v rpmbuild >/dev/null || { dnf -q -y install rpm-build >/dev/null 2>&1 || zypper -q -n install rpm-build >/dev/null; }; bash /src/scripts/linux/build-rpm.sh'

RPM="$(ls "$RPM_OUT"/windinput-*.rpm)"
ARCH="$(uname -m)"
for img in $RPM_IMAGES; do
    echo "[rpm] 核对 + 安装冒烟：$img"
    docker run --rm -v "$REPO:/src:ro" -v "$RPM:/pkg.rpm:ro" "$img" \
        bash -c 'command -v cpio >/dev/null && command -v readelf >/dev/null || { dnf -q -y install cpio binutils >/dev/null 2>&1 || zypper -q -n install cpio binutils >/dev/null; }
                 bash /src/scripts/linux/verify-rpm.sh /pkg.rpm '"$ARCH"' && bash /src/scripts/linux/smoke-rpm.sh /pkg.rpm'
done
cp "$RPM" "$OUT_DIR/"
echo "[rpm] 完成："; ls -la "$OUT_DIR"/windinput-*.rpm
