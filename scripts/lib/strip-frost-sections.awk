# 按分组标题过滤 rime-frost 的 OpenCC 杂项表：被剔除的组从其标题到下一个标题整段去掉，
# 其余行原样保留（运行时按分组取，见 emojidict::select_sections）。
#
# 参数：-v excl="<空格分隔的分组名>"。标题行形如
# `----------偏旁部首<Tab>----------`（OpenCC 无注释语法，rime-frost 借假词条写标题）。
#
# ⚠ 与 Nix 打包共用：`wind_linux/nix/data.nix` 直接引用本文件，改规则只需改这里。
#   `scripts/dev.ps1` 的 Copy-FrostOthers 与 `scripts/mac/dev.sh` 的 strip_frost_sections
#   仍是各自的实现（PowerShell / 独立脚本），改规则时要一并同步。
BEGIN { n = split(excl, ex, " ") }
/^----------[^\t]*\t-+[ \t]*$/ {
    name = $0; sub(/^-+/, "", name); sub(/\t.*$/, "", name)
    skip = 0
    for (i = 1; i <= n; i++) if (name == ex[i]) skip = 1
}
!skip { print }
