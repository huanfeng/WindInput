#!/usr/bin/env bash
# 发布前硬门禁：核对一个 .rpm 是否「真是这个架构的、基线 glibc 够低的、东西齐的」。在 rpm 系容器里跑
# （需要 rpm / rpm2cpio / cpio / readelf），与 verify-deb.sh 同口径。
#
#   scripts/linux/verify-rpm.sh <rpm> [期望架构：x86_64|aarch64，默认取 rpm 自己的 Arch]
set -euo pipefail

RPM="${1:?用法: verify-rpm.sh <rpm> [x86_64|aarch64]}"
[[ -f "$RPM" ]] || { echo "找不到 $RPM" >&2; exit 1; }
WANT="${2:-}"
MAX_GLIBC="2.35"

fail=0
bad() { echo "  ✗ $*" >&2; fail=1; }
ok()  { echo "  ✓ $*"; }

T="$(mktemp -d)"
trap 'rm -rf "$T"' EXIT
R="$T/root"; mkdir -p "$R"
# 先落成文件再解：管道里 cpio 读到归档尾就退出，pipefail 下 rpm2cpio 吃 SIGPIPE 会让整条判失败
rpm2cpio "$RPM" > "$T/pkg.cpio"
(cd "$R" && cpio -idm --quiet < "$T/pkg.cpio")

echo "== $RPM"
ARCH="$(rpm -qp --qf '%{ARCH}' "$RPM")"
[[ -n "$WANT" ]] || WANT="$ARCH"
if [[ "$ARCH" == "$WANT" ]]; then ok "Arch = $ARCH"; else bad "Arch 是 $ARCH，期望 $WANT"; fi
case "$WANT" in
    x86_64)  ELF_MACHINE="Advanced Micro Devices X86-64" ;;
    aarch64) ELF_MACHINE="AArch64" ;;
    *) echo "不认识的架构: $WANT" >&2; exit 2 ;;
esac

# 依赖：fcitx5 本体与最低版本手写，链接期依赖由 rpm 按 ELF 自动算——缺了说明自动依赖被关掉了
reqs="$(rpm -qp --requires "$RPM")"
for d in 'fcitx5 >= 5.0.14' 'libFcitx5Core.so.7' 'libstdc++.so.6' 'libc.so.6' 'fontconfig'; do
    grep -qF -- "$d" <<<"$reqs" || bad "Requires 缺 $d"
done

# 关键文件（addon 必须在 %{_libdir}/fcitx5，即 rpm 系的 lib64，不是 Debian 多架构目录）
for f in usr/lib/windinput/wind_input usr/lib/windinput/wind_setting \
         usr/lib64/fcitx5/libwindinput.so \
         usr/share/fcitx5/addon/windinput.conf usr/share/fcitx5/inputmethod/windinput.conf \
         usr/share/applications/windinput-setting.desktop usr/share/mime/packages/windinput.xml \
         usr/lib/windinput/data/compat.toml usr/share/licenses/windinput/LICENSE; do
    [[ -e "$R/$f" ]] || bad "缺文件: $f"
done
[[ ! -e "$R/DEBIAN" ]] || bad "包里混进了 DEBIAN/"
if [[ -d "$R/usr/lib/x86_64-linux-gnu" || -d "$R/usr/lib/aarch64-linux-gnu" ]]; then bad "残留 Debian 多架构目录"; fi
if readelf -d "$R/usr/lib64/fcitx5/libwindinput.so" 2>/dev/null | grep -q 'libwayland-client'; then ok "addon 带 Wayland 候选窗支持"
else bad "addon 没链 libwayland-client"; fi

# 词库（同 verify-deb.sh 的口径）
D="$R/usr/lib/windinput/data"
check_min() {
    local p="$D/$1" min="$2"
    if [[ ! -f "$p" ]]; then bad "词库缺失: $1"; return; fi
    local sz; sz=$(stat -c%s "$p")
    if (( sz < min )); then bad "词库过小(${sz}B < ${min}B): $1"; else ok "词库 $1 (${sz}B)"; fi
}
check_min schemas/pinyin/cn_dicts/base.dict.yaml 1000000
check_min schemas/wubi86/wubi86_jidian.dict.yaml 1000000
check_min schemas/wubi86/wubi86_frost.dict.yaml 100
check_min schemas/pinyin/cn_dicts/8105.dict.yaml 10000
check_min schemas/english/en.dict.yaml 1000
check_min opencc/STPhrases.octrie 100000
check_min pinyin_map.txt 10000
check_min schemas/stroke/stroke.dict.yaml 500000
check_min schemas/comments/en_zh.txt 300000
check_min schemas/comments/zh_en.txt 3000000
noncomment="$(grep -vE '^[[:space:]]*(#|$)' "$D/compat.toml" || true)"
if [[ -n "$noncomment" ]]; then bad "compat.toml 含非注释内容（应为零规则）"; else ok "compat.toml 零规则"; fi

# ELF：机器类型 + glibc 符号版本上限
n=0
while IFS= read -r f; do
    desc="$(readelf -h "$f" 2>/dev/null | awk -F: '/Machine:/ {sub(/^ +/,"",$2); print $2}' || true)"
    [[ -n "$desc" ]] || continue
    n=$((n + 1))
    rel="${f#"$R"/}"
    if [[ "$desc" != "$ELF_MACHINE" ]]; then bad "$rel 机器类型是「$desc」，期望「$ELF_MACHINE」"; continue; fi
    top="$(readelf -sW --dyn-syms "$f" 2>/dev/null | grep -o 'GLIBC_[0-9.]*' | sed 's/GLIBC_//' | sort -Vu | tail -1 || true)"
    if [[ -n "$top" ]] && [[ "$(printf '%s\n%s\n' "$top" "$MAX_GLIBC" | sort -V | tail -1)" != "$MAX_GLIBC" ]]; then
        bad "$rel 需要 GLIBC_$top（> $MAX_GLIBC）"
    else
        ok "$rel ($desc, glibc ≤ ${top:-无})"
    fi
done < <(find "$R" -type f \( -perm /111 -o -name '*.so*' \))
(( n >= 3 )) || bad "只找到 $n 个 ELF（期望 wind_input / wind_setting / libwindinput.so）"

# 权限：按 rpm 记录的模式查（cpio 解出来的受 umask 影响，不可信）。FILEMODES 是含文件类型位的十进制 st_mode。
writable=""
while read -r mode name; do
    (( (mode & 18) == 0 )) || writable+="${name}"$'\n'      # 18 = 0o022：group/other 写位
done < <(rpm -qp --qf '[%{FILEMODES} %{FILENAMES}\n]' "$RPM")
if [[ -n "$writable" ]]; then bad "存在 group/other 可写的文件："; sed 's/^/    /' <<<"$writable" >&2
else ok "无 group/other 可写文件"; fi

# scriptlet 语法
for s in POSTIN POSTUN; do
    rpm -qp --qf "%{$s}" "$RPM" > "$T/$s.sh"
    if [[ ! -s "$T/$s.sh" || "$(cat "$T/$s.sh")" == "(none)" ]]; then bad "缺 $s scriptlet"; continue; fi
    if sh -n "$T/$s.sh"; then ok "$s 语法 OK"; else bad "$s 语法错误"; fi
done

(( fail == 0 )) && echo "== 通过" || { echo "== 未通过" >&2; exit 1; }
