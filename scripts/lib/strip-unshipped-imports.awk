# 把 rime 主表 import_tables 里**没随包**的子表、以及全部 cn_dicts_cell/ 细胞词库注释掉。
#
# 参数：-v dir="<输出目录里的拼音词库目录>"。判据是「该目录里有没有 <entry>.dict.yaml」，
# 而不是写死名单——以后多发几张词库不用回来改这里。
# 用法：输出到临时文件再 mv 回去（就地改写会与读冲突，调用方负责）。
#
# ⚠ 与 Nix 打包共用：`wind_linux/nix/data.nix` 直接引用本文件，改规则只需改这里。
#   `scripts/dev.ps1` 的 Remove-UnshippedImports 与 `scripts/mac/dev.sh` 的
#   strip_unshipped_imports 仍是各自的实现（PowerShell / 独立脚本），改规则时要一并同步。
/^\.\.\.[[:space:]]*$/ { done = 1 }
!done && /^import_tables:/ { inblk = 1; print; next }
!done && inblk && /^[^[:space:]#-]/ { inblk = 0 }
!done && inblk && match($0, /^[[:space:]]*-[[:space:]]*[^[:space:]#]+/) {
    entry = substr($0, RSTART, RLENGTH); sub(/^[[:space:]]*-[[:space:]]*/, "", entry)
    if (entry ~ /^cn_dicts_cell\//) {
        print "  # [扩展词库] " substr($0, RSTART + RLENGTH - length(entry)); next
    }
    if (system("test -f \"" dir "/" entry ".dict.yaml\"") != 0) {
        print "  # [未随包] " substr($0, RSTART + RLENGTH - length(entry)); next
    }
}
{ print }
