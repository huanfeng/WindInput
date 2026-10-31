# 把 rime_frost.dict.yaml 的 import_tables 里未随包的子表与全部 cn_dicts_cell/ 注释掉。
# 与 scripts/dev.sh 的同名 awk 逻辑一致（dir 由 -v 传入，指向输出目录的拼音词库目录）。
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
