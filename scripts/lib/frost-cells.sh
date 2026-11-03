# rime-frost 随包细胞词库与不随包分组清单——`scripts/dev.sh` 与 `wind_linux/nix/data.nix`
# **source 同一份**，避免 Nix 包与 deb/Windows 包的词库构成漂移（两处以前各写一份）。
#
# ⚠️ 仍是各自一份、改这里时要同步的：
#   - `scripts/dev.ps1` 的 `$FrostCells` / `$FrostOthersExclude`（PowerShell，Windows 包）
#   - `scripts/mac/dev.sh` 的同名变量（macOS 包）
#   - `data/schemas/{pinyin,shuangpin}.schema.toml` 里的 `cell_*` 词库声明

# 随包附带的 rime-frost 细胞词库（上游 cn_dicts_cell/ 下的文件名，组装到 schemas/pinyin/ext/）。
# 只挑体积小、开了对主库排序几乎无影响的（共约 3.6MB，见 docs/design/schema-dict-discovery.md §8）。
FROST_CELLS="exthot idiom place history name name2 sport geography animal computer music media industry_product inputmethod chess chess2"

# rime-frost others.txt 里不随包的分组（按分组标题整组剔除）。「著名表演艺术家」是成人影星的
# 中日文名对照，即便模块默认关也不该进安装包。
FROST_OTHERS_EXCLUDE="著名表演艺术家"
