# 注释增量提示（pre-commit 调用，只提示不拦截）。输入：git diff -U0，只看新增行。
# 约定见 AGENTS.md「注释与测试组织」：注释写结论，排查过程与历史进提交信息或 docs/。
function flush() {
    if (run > MAX) printf "  %s:%d 新增连续注释 %d 行（建议 ≤%d）\n", file, start, run, MAX
    run = 0
}
/^\+\+\+ / { flush(); file = substr($0, 7); next }
/^@@/ { flush(); split($3, a, ","); ln = substr(a[1], 2) + 0; next }
/^\+/ {
    s = substr($0, 2)
    if (s ~ /^[ \t]*\/\//) {
        if (run == 0) start = ln
        run++
        if (s ~ /20[0-9][0-9]-[01][0-9]-[0-3][0-9]([^T0-9]|$)|实测|靶机|改动前|GH#[0-9]/)
            printf "  %s:%d 注释含过程记录（日期/实测/靶机/编号）\n", file, ln
    } else flush()
    ln++
    next
}
END { flush() }
