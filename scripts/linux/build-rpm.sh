#!/usr/bin/env bash
# 把 stage-linux.sh 组装好的安装树打成 .rpm（Fedora / openSUSE 通用）。
# 在 rpm 系容器里跑（需要 rpmbuild）；构建本身不在这里——二进制来自 Ubuntu 22.04 基线上的
# stage-linux.sh（glibc 最低），这里只加 rpm 元数据。调用处：package-rpm.sh（本机 docker）与 linux-build.yml。
#
# 环境变量：
#   APP_VERSION   必填，产品版本（docs/VERSION 口径；预发布后缀的 - 换成 ~ 作 rpm Version）
#   STAGE_DIR     必填，安装树目录（stage-linux.sh 的产物）
#   SRC_DIR       WindInput 仓库根（默认脚本所在仓库；取 spec / LICENSE / NOTICE.md）
#   OUT_DIR       产物目录（默认 ./out），得到 windinput-<版本>-1.<arch>.rpm
#   TOP_DIR       rpmbuild 工作目录（默认 mktemp）
set -euo pipefail

: "${APP_VERSION:?}" "${STAGE_DIR:?}"
SRC_DIR="${SRC_DIR:-$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)}"
OUT_DIR="${OUT_DIR:-$PWD/out}"
TOP_DIR="${TOP_DIR:-$(mktemp -d)}"
command -v rpmbuild >/dev/null || { echo "需要 rpmbuild（在 rpm 系容器里跑）" >&2; exit 1; }
[[ -d "$STAGE_DIR/usr/lib/windinput" ]] || { echo "STAGE_DIR 不是安装树: $STAGE_DIR" >&2; exit 1; }
mkdir -p "$OUT_DIR" "$TOP_DIR"/{BUILD,RPMS,SPECS,SRPMS}

RPM_VERSION="${APP_VERSION/-/"~"}"     # 预发布后缀用 ~，排在同号正式版之前（与 deb 同理）
rpmbuild -bb "$SRC_DIR/scripts/linux/pkg/windinput.spec" \
    --define "_topdir $TOP_DIR" \
    --define "wind_version $RPM_VERSION" \
    --define "stage_dir $(readlink -f "$STAGE_DIR")" \
    --define "src_dir $(readlink -f "$SRC_DIR")"
cp "$TOP_DIR"/RPMS/*/windinput-*.rpm "$OUT_DIR/"
ls -la "$OUT_DIR"/windinput-*.rpm
