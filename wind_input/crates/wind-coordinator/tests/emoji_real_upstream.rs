//! emoji 扩展表的**真实上游数据**端到端验证：解析 → 繁简归一 → 建 `.wemj` → mmap 查表。
//!
//! 单元测试用的是构造样本（`emojidict` 内的 13 条、`plan_emoji_insertions` 的 8 条），
//! 它们验的是判据；本文件验的是**那些判据碰上真数据时还成不成立**——上游 4668 行全繁体
//! 键、12 组归一撞键、两表 14 个同键，这些形态构造不出来也想不全。
//!
//! 数据来自 `build_dev/data/`（`scripts/dev.* gen-data` 产出），缺失时**跳过**：CI 无数据
//! 环境跑不了，而为它引一份测试夹具等于把 126KB 上游词表复制进仓库（还是 LGPL 的）。
//!
//! ⚠️ 跳过是静默的（同 `wind-dict/tests/real_dict.rs` 的既有教训：那边曾因路径写错两级而
//! 长期空跑、计数照绿）。故本文件在跳过时**打印原因**，且断言里带上具体数字——真跑起来
//! 时数字对不上会当场报出来，不会退化成「跑了但什么都没断言」。

use std::path::PathBuf;

use wind_dict::emojidict;
use wind_transform::s2t;

/// 仓库根的 `build_dev/data`。
///
/// 三级向上：`crates/wind-coordinator` → `crates` → `wind_input` → 仓库根。
/// （`real_dict.rs` 记着这里踩过「只写两级」的坑，直接按它修正后的写法来。）
fn data_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../build_dev/data")
}

#[test]
fn emoji_real_upstream_end_to_end() {
    let data = data_dir();
    let word = data.join("emoji/emoji_word.txt");
    let category = data.join("emoji/emoji_category.txt");
    let octrie = data.join("opencc/TSCharactersDerived.octrie");
    if !word.is_file() || !octrie.is_file() {
        eprintln!(
            "跳过 emoji 真实数据验证：缺 {} 或 {}（运行 scripts/dev.* 的 gen-data）",
            word.display(),
            octrie.display()
        );
        return;
    }

    let norm = s2t::Dict::load(&octrie).expect("繁简归一表解析失败");
    let tmp = std::env::temp_dir().join(format!("windinput-emoji-e2e-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&tmp);
    let cache = tmp.join("emoji.wemj");

    let tables = vec![word, category];
    let dict = emojidict::load_or_build(&tables, &[octrie], &cache, |s| norm.convert_once(s))
        .expect("真实数据建表失败");

    // ── 1. 繁简归一确实生效 ────────────────────────────────────────────────
    // 上游键全是繁体（实测 国0/國28、爱0/愛16）。查得到简体键 ⇒ 归一这一步真跑了。
    // 不归一的话这几条全是 None，而功能会「看着已启用、实际一条都不出」。
    for w in ["一个人", "中华", "台湾"] {
        assert!(
            dict.lookup(w).is_some(),
            "简体键 {w} 查不到 —— 繁简归一没生效"
        );
    }
    // 反向：原繁体键**不该**还在（归一是替换，不是追加）。
    assert!(dict.lookup("一個人").is_none(), "繁体原键不该保留");

    // ── 2. 两表合并且同键不覆盖 ────────────────────────────────────────────
    // 「奖项」在 word 表只有 🏅，在 category 表是一整组；合并后必须两边都在。
    // 这是 write_emoji_wemj 里「同键合并而非覆盖」那条唯一会真丢数据的场景。
    let prize = dict.lookup("奖项").expect("奖项 应在两表之一命中");
    let n = prize.split_whitespace().count();
    assert!(
        n > 1,
        "奖项 只有 {n} 个 emoji（{prize}）—— 两表同键被覆盖了，没有合并"
    );

    // ── 3. 规模合理 ────────────────────────────────────────────────────────
    // 实测 word 4657 键 + category 166 键、交集 14 ⇒ 合并后约 4809。给一个宽区间：
    // 精确数字会随上游更新而变，但掉到几百或涨到几万都说明解析出了问题。
    let count = dict.entry_count();
    assert!(
        (4000..6000).contains(&count),
        "条目数 {count} 不在合理区间 —— 解析或合并有问题"
    );

    // ── 4. 值形态 ──────────────────────────────────────────────────────────
    // 值是空格分隔的 emoji，**不含**上游那个与键相同的首项（`parse_upstream` 按位置丢弃）。
    let animals = dict.lookup("动物").expect("分类表应已合并进来");
    assert!(
        !animals.split_whitespace().any(|e| e == "动物"),
        "值里混进了键本身 —— 首项没被丢掉"
    );

    let _ = std::fs::remove_dir_all(&tmp);
}
