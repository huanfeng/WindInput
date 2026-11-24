//! 码表整句在真实五笔词库上的表现探针。
//!
//! `codetable::sentence` 的单元测试用手写词条证明算法性质（简码判定、边数惩罚、3 码全码参与），
//! 证明不了在真实词库上连打一串码会得到什么；`SHORT_CODE_PENALTY` 等参数的标定也要靠这里。
//!
//! ```text
//! cargo test -p wind-engine --test codetable_sentence_probe -- --ignored --nocapture
//! ```
//!
//! 拿单个词条的码回测它能不能被切出来恒真、毫无信息，故本探针拼接多个词，要求解码器在大量切分里
//! 选中按词边界切的那一条。但词序是拼出来的而非真实语料，还原率只是下限指标，不能当准确率报。
//!
//! 当前基线：全码 12/12、简码 10/12。简码剩下两条是真实歧义而非选错字（`qjkhlgw` 的二简「旬」与
//! 「我」+「是」击键同形；`gwhvbrjfwh` 的「修好」是真实词组），要上下文模型或手动分隔符才能分辨，
//! 不是调惩罚值能修的。「准确率不够」时先查词频量纲、再调惩罚值。见
//! docs/design/codetable-sentence-input.md 的「11.9」「11.12」。

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use wind_dict::cached::CachedDict;
use wind_dict::{DictManager, SystemDictLayer};
use wind_engine::codetable::{CodeTableEngine, CommitOptions};

const MAX_CODE_LEN: usize = 4;

fn data_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../build_dev/data/schemas")
}

fn wubi_dict() -> Option<CachedDict> {
    let base = data_dir().join("wubi86");
    let yaml = base.join("wubi86_jidian.dict.yaml");
    if !yaml.exists() {
        return None;
    }
    let wdat = base.join("wubi86_jidian.dict.wdat");
    CachedDict::load_at_with(&yaml, &wdat, false).ok()
}

/// `(引擎, text→全码, text→最短码)`。
///
/// 两张码表分别模拟打全码（整句的理想输入）与打简码（五笔用户的肌肉记忆），
/// 后者正是「智能容忍」要承担的那一档。
type Fixture = (
    CodeTableEngine,
    HashMap<String, String>,
    HashMap<String, String>,
);

fn fixture() -> Option<Fixture> {
    let dict = wubi_dict()?;
    let mut full: HashMap<String, String> = HashMap::new();
    let mut short: HashMap<String, String> = HashMap::new();
    dict.for_each_entry(&mut |code, text, _w| {
        if !code.chars().all(|c| c.is_ascii_lowercase()) {
            return;
        }
        full.entry(text.to_string())
            .and_modify(|c| {
                if code.len() > c.len() {
                    *c = code.to_string();
                }
            })
            .or_insert_with(|| code.to_string());
        short
            .entry(text.to_string())
            .and_modify(|c| {
                if code.len() < c.len() {
                    *c = code.to_string();
                }
            })
            .or_insert_with(|| code.to_string());
    });

    let dm = DictManager::new();
    dm.register_layer(Box::new(SystemDictLayer::new(dict, "codetable-system")));
    let opts = CommitOptions {
        sentence_input: true,
        ..Default::default()
    };
    // 走生产同一条路径：只交数据目录，词库由解码器在首次解码时自己加载
    // （`FreqSource::SchemasDir`），预先加载再注入会让懒加载链路一行都测不到。
    let e = CodeTableEngine::new(MAX_CODE_LEN, opts, Arc::new(dm))
        .with_sentence_schemas_dir(data_dir());
    Some((e, full, short))
}

/// 待测句子：拆成词，由夹具查码拼接。
///
/// 必须含单字：五笔词组恒 4 码，最短码就是它自己，全是多字词时两种打法结果相同、测不到简码。
/// 单字才有 1/2/3 码的简码位，也才是整句真正的难点。
const SENTENCES: &[&[&str]] = &[
    // 纯词组：切分只需在 4 码边界上对齐
    &["工作", "人员"],
    &["计算机", "程序"],
    &["经济", "发展", "水平"],
    &["技术", "创新"],
    &["管理", "系统"],
    &["这个", "问题"],
    // 含单字：简码打法下「人 w」「是 j」「的 r」「一 g」都只占 1 码，
    // 解码器要在「这 1 个字母是一个字」和「它是下一个词的首码」之间选
    &["中国", "人"],
    &["我", "是", "中国", "人"],
    &["大家", "好"],
    &["一", "个", "好", "的", "时候"],
    &["学习", "工作", "生活"],
    &["我们", "可以", "看到"],
];

fn codes_for(words: &[&str], table: &HashMap<String, String>) -> Option<String> {
    let mut s = String::new();
    for w in words {
        s.push_str(table.get(*w)?);
    }
    Some(s)
}

/// 整句解的切分分段（诊断用）。空 = 没解出整句。
fn segments(e: &CodeTableEngine, code: &str) -> Vec<String> {
    e.sentence_segments(code).unwrap_or_default()
}

fn top_text(e: &CodeTableEngine, code: &str) -> String {
    use wind_engine::Engine;
    e.convert(code, 20)
        .ok()
        .and_then(|r| r.candidates.first().map(|c| c.text.clone()))
        .unwrap_or_default()
}

/// 主探针：两种打法各跑一遍，打印首选与还原率。
#[test]
#[ignore = "探针：依赖 build_dev 真实词库"]
fn sentence_on_real_wubi_dict() {
    let Some((e, full, short)) = fixture() else {
        eprintln!("!!! 跳过：build_dev 五笔词库不存在");
        return;
    };

    for (label, table) in [("全码", &full), ("最短码(简码)", &short)] {
        let mut hit = 0usize;
        let mut total = 0usize;
        println!("\n=== 打法：{label} ===");
        for words in SENTENCES {
            let Some(code) = codes_for(words, table) else {
                println!("  (跳过，词库缺词) {words:?}");
                continue;
            };
            let want: String = words.concat();
            let got = top_text(&e, &code);
            total += 1;
            if got == want {
                hit += 1;
            }
            // 失败时打印切分分段：「切错了」与「同码选错了」是两类不同的问题，只看首选文本看不出。
            let seg = if got == want {
                String::new()
            } else {
                format!("   切分={:?}", segments(&e, &code))
            };
            println!(
                "  {:<28} {:<12} → {:<14} {}{}",
                code,
                want,
                got,
                if got == want { "✓" } else { "✗" },
                seg
            );
        }
        println!("  还原率 {hit}/{total}");
        // 回归闸门：全码 12/12、简码 10/12（失败归因见文件头）。低于基线说明有回退，
        // 不要直接改这个数字——先看打印出来的切分分段，确认失败的是不是同一批用例。
        let floor = if label == "全码" { 12 } else { 10 };
        assert!(
            hit >= floor,
            "{label} 还原率 {hit}/{total} 低于基线 {floor}，见文件头归因表"
        );
    }
}

/// 手动分隔符能不能救回歧义——「智能容忍 + 显式分隔符」路线的核心主张，必须在真实词库上验证。
///
/// 两个现场取自主探针剩下的失败用例：`qj` 既是二简「旬」也是「我 q」+「是 j」；`whvb` 既是
/// 「修好」也是「个 wh」+「好 vb」。打分无从区分，只有用户能表态。
#[test]
#[ignore = "探针：依赖 build_dev 真实词库"]
fn manual_separator_resolves_real_ambiguity() {
    let Some((e, _full, short)) = fixture() else {
        eprintln!("!!! 跳过：build_dev 五笔词库不存在");
        return;
    };
    let sep = "'";
    println!(
        "
=== 手动分隔符消歧（真实词库）==="
    );
    let cases: &[(&str, &str)] = &[
        // 我 q | 是 j | 中国 khlg | 人 w —— 不加分隔符时 `qj` 被读成二简「旬」
        (&format!("q{sep}j{sep}khlg{sep}w"), "我是中国人"),
        // 一 g | 个 wh | 好 vb | 的 r | 时候 jfwh —— 不加时 `whvb` 被读成词「修好」
        (&format!("g{sep}wh{sep}vb{sep}r{sep}jfwh"), "一个好的时候"),
    ];
    let mut hit = 0;
    for (code, want) in cases {
        let got = top_text(&e, code);
        let ok = got == *want;
        if ok {
            hit += 1;
        }
        println!(
            "  {:<26} {:<12} → {:<14} {}",
            code,
            want,
            got,
            if ok { "✓" } else { "✗" }
        );
        if !ok {
            println!("      切分={:?}", segments(&e, code));
        }
    }
    // 顺带证明「不加分隔符时确实会错」——否则这条探针可能在两种情况下都绿，
    // 那样它就证明不了分隔符起了作用。
    let joined: String = cases[0].0.replace(sep, "");
    let joined_got = top_text(&e, &joined);
    println!("  对照（不加分隔符）: {joined} → {joined_got}");
    assert_ne!(
        joined_got, "我是中国人",
        "前提自检：不加分隔符时本该出歧义解，若这里也对了，本探针就证明不了分隔符的作用"
    );
    assert_eq!(hit, cases.len(), "分隔符应能救回全部歧义用例");
    let _ = short;
}

/// 简码索引在真实词库上的规模——对账设计文档 §2.3 的数字。
///
/// 期望：一简 50 条全是简码、二简 654/655、三简 3748/5352 ⇒ 合计 4452 条左右。
/// 数量级对不上说明判据在真实数据上没按预期工作（比如容错码让「最长码」判定漂了）。
#[test]
#[ignore = "探针：依赖 build_dev 真实词库"]
fn short_code_index_scale_on_real_dict() {
    use wind_engine::codetable::ShortCodeIndex;
    let Some(dict) = wubi_dict() else {
        eprintln!("!!! 跳过：build_dev 五笔词库不存在");
        return;
    };
    let dm = DictManager::new();
    dm.register_layer(Box::new(SystemDictLayer::new(dict, "codetable-system")));

    let t0 = std::time::Instant::now();
    let idx = ShortCodeIndex::build(&dm);
    let ms = t0.elapsed().as_millis();

    println!("\n=== 简码索引（真实五笔词库）===");
    println!("  简码条目 {} 条，构建耗时 {ms} ms", idx.len());
    // 一简/二简的代表：极点词库里「一 g」「工 a」恒是简码。
    println!("  a→工 {:?}", idx.resolve("a", "工", 9999));
    println!("  g→一 {:?}", idx.resolve("g", "一", 9999));
    // 3 码全码的代表：「皮 hci」没有更长码，不该进表。
    println!("  hci→皮 {:?}", idx.resolve("hci", "皮", 1200));

    assert!(
        (3500..6000).contains(&idx.len()),
        "简码条目数 {} 偏离预期区间（设计文档 §2.3 实测约 4452）",
        idx.len()
    );
}
