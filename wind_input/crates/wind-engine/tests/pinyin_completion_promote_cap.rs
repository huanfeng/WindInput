//! 近距离前缀补全的上浮名额：别让「词淹掉单字」。
//!
//! `distance <= 1` 的补全（补完手头正在输入的那个音节）**无条件上浮**进完整匹配层，
//! 本意是让「没有」这种高频补全别被数百条同音单字淹掉。但它对**同距离的一大批**候选
//! 没有任何约束 —— 真机 `meiy` 的残码 `y` 一个音节就补出 58 条，全部 distance=1、
//! 全部上浮，于是单字「没」被整批压到第 59 位。方向恰好翻转，问题照旧。
//!
//! 自带 wdat 夹具，不依赖 `build_dev/data`（那类测试在词库缺失时静默跳过、计数照常绿）。

use wind_dict::cached::CachedDict;
use wind_dict::datformat::WdatWriter;
use wind_engine::Engine;
use wind_engine::pinyin::{Config as PyConfig, PinyinEngine};

/// 20 条 `meiy?` 的双音节词 + 1 个单字「没」。
///
/// 20 这个数量是要害：它**大于**名额（8）、小于真机那 58 条 —— 足以让「无名额」与
/// 「有名额」两种行为分开，又不至于让夹具变成一本词典。权重递减，模拟真实词库的长尾。
fn fixture(tag: &str) -> CachedDict {
    let dir = std::env::temp_dir().join(format!("wind_promote_cap_{tag}"));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let wdat = dir.join("t.wdat");
    let mut w = WdatWriter::new();
    // 单字：`mei` 是 `meiy` 的真前缀 ⇒ 精确子串候选（is_partial），层级上本就沉在
    // 上浮后的补全之后 —— 正因如此，上浮的数量必须有限。
    w.add_with_boundary("mei".into(), vec![("没".into(), 50000, 0, 0b1)]);
    // 20 条双音节补全，码形如 meiya/meiyb/…，boundary 均为 mei|y? ⇒ distance = 1。
    for (i, ch) in "abcdefghijklmnopqrst".chars().enumerate() {
        let code = format!("meiy{ch}");
        let text = format!("没{ch}");
        // ⚠️ **权重序与码字典序刻意相反**：码越大权重越高（`meiyt` 最高、`meiya` 最低）。
        // 两序一致的话，「按遍历序发名额」与「按权重发名额」结果相同，
        // `promotion_quota_goes_to_highest_weight` 就测不出差异 —— 初版正是如此，
        // 把排序那行删掉照样绿。
        let weight = 1000 + (i as i32) * 500;
        // ⚠️ boundary 是 `mei|y?` 的起点 **0 和 3**（`0b1001`）。写成 0b10001(0 和 4) 会切成
        // `meiy|?`，而 `meiy` 不是合法音节 —— `prefix_syllable_aligned` 要求 `completed_len`
        // 那一位(3)置位，不置位整批候选会被静默过滤掉，测试就退化成「候选只有单字」的假绿。
        w.add_with_boundary(code, vec![(text, weight, i as u32, 0b1001)]);
    }
    w.write(&wdat).unwrap();
    CachedDict::load_at(&dir.join("t.dict.yaml"), &wdat).expect("加载 wdat 夹具")
}

fn engine(tag: &str) -> PinyinEngine {
    PinyinEngine::new(PyConfig::default(), fixture(tag))
}

/// 单字不得被整批上浮的补全压到候选面之外。
///
/// 无名额时 20 条补全全部上浮 ⇒ 单字排第 21；有名额（8）时单字排第 9。
/// 断言取 12 而不是精确的 9：名额值是可调的取舍，护栏要守的是「有上限」这件事本身，
/// 钉死在具体位次上会让每次微调都误报。
#[test]
fn near_completion_promotion_is_capped() {
    let e = engine("cap");
    let r = e.convert("meiy", 100).expect("convert 成功");
    // 前提自检：夹具确实产出了**多于名额**的补全，否则 `pos < 12` 在「候选只有单字」时
    // 恒真，这条就成了假护栏（初版正是如此：boundary 写错导致补全全被过滤，照样绿）。
    let completions = r.candidates.iter().filter(|c| c.is_prefix).count();
    assert!(
        completions > 8,
        "前提：夹具须产出多于名额(8)的近距离补全，实际 {completions} 条"
    );
    let pos = r
        .candidates
        .iter()
        .position(|c| c.text == "没")
        .unwrap_or_else(|| {
            panic!(
                "单字「没」应在候选里: {:?}",
                r.candidates.iter().map(|c| &c.text).collect::<Vec<_>>()
            )
        });
    assert!(
        pos < 12,
        "单字被上浮的补全压到第 {} 位 —— 近距离上浮须有名额，前 12: {:?}",
        pos + 1,
        r.candidates
            .iter()
            .take(12)
            .map(|c| &c.text)
            .collect::<Vec<_>>()
    );
}

/// 没抢到名额的补全**沉到单字之后**，而不是留在上层。
///
/// ⚠️ 本用例**测不到「名额按权重发」**：最终 `sort_by` 会按 `cmp_match_layers.then(weight)`
/// 重排，而上游 `search_prefix_*` 返回的本就是权重序 —— 把引擎里那行排序删掉，本用例照样绿
/// （已变异验证）。它守的是「有没有名额」这件事：无名额时 20 条全上浮，低权重那条会排在
/// 单字之前。测不到的那一半见引擎侧那段 ⚠️ 注释。
#[test]
fn unpromoted_completions_sink_below_single_char() {
    let e = engine("byweight");
    let r = e.convert("meiy", 100).expect("convert 成功");
    // 权重最低的「没a」（码最小、遍历序第一）抢不到名额，须沉到单字之后。
    let pos_mei = r.candidates.iter().position(|c| c.text == "没").unwrap();
    let pos_lowest = r.candidates.iter().position(|c| c.text == "没a").unwrap();
    assert!(
        pos_lowest > pos_mei,
        "没抢到名额的低权重补全应沉到单字之后：没a 第 {}，没 第 {}",
        pos_lowest + 1,
        pos_mei + 1
    );
}
