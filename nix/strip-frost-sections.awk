# 按分组标题过滤 rime-frost 的 OpenCC 杂项表：被剔除的组从其标题到下一个标题整段去掉。
# 与 scripts/dev.sh 的同名 awk 逻辑一致（excl 由 -v 传入，空格分隔的分组名）。
BEGIN { n = split(excl, ex, " ") }
/^----------[^\t]*\t-+[ \t]*$/ {
    name = $0; sub(/^-+/, "", name); sub(/\t.*$/, "", name)
    skip = 0
    for (i = 1; i <= n; i++) if (name == ex[i]) skip = 1
}
!skip { print }
