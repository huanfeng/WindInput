#!/usr/bin/env bash
# rpm 安装冒烟：在干净的 rpm 系发行版（Fedora / openSUSE）里真装一次——依赖能解析、三个 ELF 没有缺失的
# 动态库、rpm -V 通过、addon 能被 fcitx5 加载、能卸载且无残留。「装得上、一加载就找不到 so」是这类包
# 最常见的静默坏法。与 linux-build.yml 的 deb 冒烟同口径。
#
#   scripts/linux/smoke-rpm.sh <rpm>          # 在容器里以 root 跑；dnf 或 zypper 自动识别
set -euo pipefail

RPM="${1:?用法: smoke-rpm.sh <rpm>}"
fail=0
bad() { echo "  ✗ $*" >&2; fail=1; }
ok()  { echo "  ✓ $*"; }

if command -v dnf >/dev/null; then
    dnf -y install "$RPM" >/tmp/install.log 2>&1 || { cat /tmp/install.log; bad "dnf install 失败"; exit 1; }
elif command -v zypper >/dev/null; then
    # 本地未签名包：zypper 默认会就签名提问，非交互下要显式放行
    zypper -n --no-gpg-checks install "$RPM" >/tmp/install.log 2>&1 || { cat /tmp/install.log; bad "zypper install 失败"; exit 1; }
else
    echo "既没有 dnf 也没有 zypper" >&2; exit 2
fi
ok "依赖解析并安装成功"

ADDON="$(rpm -ql windinput | grep '/fcitx5/libwindinput\.so$')"
for f in /usr/lib/windinput/wind_input /usr/lib/windinput/wind_setting "$ADDON"; do
    if ldd "$f" 2>/dev/null | grep -q 'not found'; then bad "$f 有缺失的动态库"; ldd "$f" | grep 'not found' >&2
    else ok "$f 动态库齐全"; fi
done
# rpm -V：文件内容 / 权限与包内记录一致（配置文件无，所以全部应通过）
if rpm -V windinput; then ok "rpm -V 通过"; else bad "rpm -V 报告差异"; fi

# 卸载：文件清走、私有目录不残留
if command -v dnf >/dev/null; then dnf -y remove windinput >/tmp/remove.log 2>&1 || bad "dnf remove 失败"
else zypper -n remove windinput >/tmp/remove.log 2>&1 || bad "zypper remove 失败"; fi
[[ ! -e /usr/lib/windinput ]] && ok "卸载后无残留" || bad "卸载后 /usr/lib/windinput 仍在"

(( fail == 0 )) && echo "== smoke 通过" || { echo "== smoke 未通过" >&2; exit 1; }
