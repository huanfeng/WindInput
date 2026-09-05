//! 生成 `data/charsets/blocks.yaml`：50 个 Unicode 区块 + 「其它」补集 + 预设组「符号」。
//!
//! ```text
//! cargo run -q -p wind-tools --bin gen_block_charsets -- --out ../data/charsets/blocks.yaml
//! ```
//!
//! # 为什么区块表要出成配置
//!
//! 这套字符类系统的全部目的就是让判据可自定义。区块若钉在代码里，就成了「唯独这部分
//! 不可配」——同一个错误在 emoji 上犯过一次（设计文档 §5.2：「不能用 ranges」被错误地
//! 推成「必须内置」）。
//!
//! # 为什么仍由生成器产出，而不是手写
//!
//! 区块表随 Unicode 升版增长，且「其它」是**补集**——手写要人肉算 19 段空隙，改一块就得
//! 重算。生成器直接读 [`wind_candidate::block_table`]，两份数据因此同源；
//! `factory_blocks_match_the_block_table_codepoint_by_codepoint` 逐码位钉住它们不漂移。

use std::io::Write;

/// 区块类的 `order`：比 emoji(10) / 常用汉字(50) / 用户自建类(默认 100) 都靠后。
///
/// 区块类不表态，本值只影响「一个字符展示成哪个类」，让具体的块排在预设组「符号」之前。
const BLOCK_ORDER: i32 = 900;

/// 预设组「符号」的 `order`，必须**大于** [`BLOCK_ORDER`]：否则半个 BMP 的类型列都会
/// 显示成「符号」而不是具体块名。
const PRESET_ORDER: i32 = 1000;

const MAX_CODEPOINT: u32 = 0x10FFFF;

fn main() -> anyhow::Result<()> {
    let mut out = String::new();
    let args: Vec<String> = std::env::args().collect();
    let path = args
        .windows(2)
        .find(|w| w[0] == "--out")
        .map(|w| w[1].clone())
        .unwrap_or_else(|| "../data/charsets/blocks.yaml".to_string());

    let blocks = wind_candidate::block_table();
    let other = wind_candidate::other_block_name();
    let symbols = wind_candidate::preset_symbol_block_names();

    out.push_str(HEADER);
    for (name, lo, hi) in blocks {
        out.push_str(&format!(
            "\n- key: {name}\n  ranges: [{}]\n  order: {BLOCK_ORDER}\n",
            range_text(*lo, *hi)
        ));
    }

    out.push_str(OTHER_NOTE);
    out.push_str(&format!("- key: {other}\n  ranges:\n"));
    for (lo, hi) in complement(blocks) {
        out.push_str(&format!("    - {}\n", range_text(lo, hi)));
    }
    out.push_str(&format!("  order: {BLOCK_ORDER}\n"));

    out.push_str(SYMBOL_NOTE);
    out.push_str("- key: 符号\n  ranges:\n");
    let mut n = 0;
    for (name, lo, hi) in blocks {
        if symbols.contains(name) {
            out.push_str(&format!("    - {}\n", range_text(*lo, *hi)));
            n += 1;
        }
    }
    // ⚠️ 组员写错一个名字，那一块就**静默消失**：用户勾了「符号」，某片字符不生效，
    // 而配置校验一声不吭。在生成时就核对数量，比留到运行期强。
    anyhow::ensure!(
        n == symbols.len(),
        "预设组「符号」有 {} 个成员名在区块表里找不到",
        symbols.len() - n
    );
    out.push_str(&format!("  order: {PRESET_ORDER}\n"));

    let mut f = std::fs::File::create(&path)?;
    f.write_all(out.as_bytes())?;
    println!(
        "已写出 {path}：{} 个区块 + 「{other}」+ 符号({n} 段)",
        blocks.len()
    );
    Ok(())
}

fn range_text(lo: u32, hi: u32) -> String {
    if lo == hi {
        format!("U+{lo:04X}")
    } else {
        format!("U+{lo:04X}-U+{hi:04X}")
    }
}

/// 区块表的补集。依赖块表**按 start 升序且互不重叠**（`blocks_are_sorted_and_disjoint`
/// 钉着这条，它同时是 `block_index_of` 二分查找的正确性前提）。
fn complement(blocks: &[(&str, u32, u32)]) -> Vec<(u32, u32)> {
    let mut out = Vec::new();
    let mut next = 0u32;
    for (_, lo, hi) in blocks {
        if *lo > next {
            out.push((next, lo - 1));
        }
        next = hi.saturating_add(1);
    }
    if next <= MAX_CODEPOINT {
        out.push((next, MAX_CODEPOINT));
    }
    out
}

const HEADER: &str = "\
---
# 内置 Unicode 区块类 —— **本文件由 `gen_block_charsets` 生成，改动会被下次生成覆盖。**
#
# 这些类**一个判定字段都不表态**：它们存在的理由只是给 `schema.frequency.exclude_blocks`
# 与 `input.rare_char.include_blocks` 里那些名字一个落点，并给设置页的类型列一个标签。
# 任何一个表了态，都会在用户什么都没配的情况下改变候选的常用性判定。
#
# 要给某一块加属性（比如让「表情符号」块免词频），**不要改本文件**：去设置页
# 「字符集分类」改那一行，或写一个只含 `key: 表情符号` 加你要的字段的 .yaml 用
# 「从文件加载」导入——覆盖按 key 匹配、字段级合并，块的 ranges 会保留。
";

const OTHER_NOTE: &str = "
# 「其它」= 区块表**之外**的一切，即上面所有区间的补集。
#
# ★ 它存在的理由：区块表逐块列举一份仍在增长的 Unicode 区间，新版本的新块必然落进这里。
# 对**准入**类消费者（生僻字模式）而言，漏一块 = 那批字打不出——不安全的方向。
# 给出这一档，新块就落进一个用户控制得到的开关，而不是静默消失。
";

const SYMBOL_NOTE: &str = "
# 预设组「符号」—— 标点、数学、图形这一类**非 emoji** 的符号块的并集。
#
# ⚠️ 与 emoji 刻意不相交：勾「符号」不该顺带把 emoji 也放进来，否则界面上两个开关的
# 关系说不清楚。emoji 不在这里——它由 charsets/emoji.yaml 那份按 UTS #51 生成的精确
# 字表给出（「五个块并集」的老口径两个方向都不准，见设计文档 §5.5）。
";
