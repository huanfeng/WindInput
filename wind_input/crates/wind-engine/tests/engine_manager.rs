//! 引擎管理器端到端测试
//!
//! 用仓库内真实 schema 构建 EngineManager，验证五笔/拼音转换产出候选。
//! 词典缺失时自动跳过。

use std::path::PathBuf;
use wind_config::Config;
use wind_engine::EngineManager;

fn data_dir() -> PathBuf {
    // 三级：crates/wind-engine → crates → wind_input → 仓库根（build_dev 在仓库根）。
    // 曾误写成两级，解析到 wind_input/build_dev/data —— 该目录不存在，于是 schema_exists()
    // 判假、**本文件所有依赖真实词库的测试静默走「跳过」分支通过**。判据是耗时 0.00s。
    // 同款坑见 wind-coordinator/tests/input_flow.rs 的同名函数（那边早已修正）。
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../build_dev/data")
}

fn make_config(schemas: &[&str]) -> Config {
    let mut cfg = Config::default();
    cfg.schema.available = schemas.iter().map(|s| s.to_string()).collect();
    cfg.schema.active = schemas[0].to_string();
    // ⚠️ 召回门槛设回旧出厂值 2 / 3（现出厂为 4 / 5）。本文件有用例以 `qingfengs`
    // （3 音节）验用户长词上浮，出厂门槛下该候选在召回层就没了。门槛本身的出厂行为由
    // `wind-coordinator` 的 `pinyin_completion_recall_gate` 守，这里不重复。
    cfg.schema.pinyin.completion.min_syllables = 2;
    cfg.schema.pinyin.completion.max_extra_syllables = 3;
    cfg
}

fn schema_exists(dir: &std::path::Path, id: &str) -> bool {
    dir.join(format!("schemas/{}.schema.toml", id)).exists()
        || dir.join(format!("schemas/{}.schema.yaml", id)).exists()
}

/// english 方案（隐藏，懒加载）：ensure_schema 应可加载，convert_with 前缀查词命中，
/// 候选来源标记为 English，且无自动上屏。词库/schema 缺失时跳过。
#[test]
fn test_english_schema_lazy_loads_and_converts() {
    // build_dev 可能位于 wind_input/build_dev（data_dir()）或产品仓根 build_dev；两处都试。
    let dir = [
        data_dir(),
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../build_dev/data"),
    ]
    .into_iter()
    .find(|d| d.join("schemas/english/en.dict.yaml").exists())
    .unwrap_or_else(data_dir);
    if !schema_exists(&dir, "english") || !dir.join("schemas/english/en.dict.yaml").exists() {
        eprintln!("跳过：english schema/词库不存在");
        return;
    }
    // 活跃方案用 wubi86；english 仅作隐藏方案懒加载（不在 available）。
    let cfg = make_config(&["wubi86"]);
    let mgr = EngineManager::new(&cfg, Some(&dir));

    assert!(mgr.ensure_schema("english"), "english 方案应可懒加载");

    let result = mgr.convert_with("english", "hel", 50);
    assert!(
        !result.candidates.is_empty(),
        "english 'hel' 应产出前缀候选"
    );
    assert!(
        result
            .candidates
            .iter()
            .any(|c| c.text.eq_ignore_ascii_case("hello")),
        "应包含 hello，实际前几个: {:?}",
        result
            .candidates
            .iter()
            .take(5)
            .map(|c| &c.text)
            .collect::<Vec<_>>()
    );
    assert!(
        result
            .candidates
            .iter()
            .all(|c| c.source == wind_candidate::CandidateSource::English),
        "english 候选来源应全部标记为 English"
    );
    assert!(!result.should_commit, "english 不应自动上屏");
}

#[test]
fn test_wubi_engine_candidates() {
    let dir = data_dir();
    if !schema_exists(&dir, "wubi86") {
        eprintln!("跳过：wubi86 schema 不存在");
        return;
    }
    let cfg = make_config(&["wubi86"]);
    let mgr = EngineManager::new(&cfg, Some(&dir));

    let result = mgr.convert("aaaa", 9);
    assert!(!result.candidates.is_empty(), "五笔 'aaaa' 应产出候选");
    assert!(
        result.candidates.iter().any(|c| c.text == "恭恭敬敬"),
        "应包含 恭恭敬敬，实际: {:?}",
        result
            .candidates
            .iter()
            .map(|c| c.text.as_str())
            .collect::<Vec<_>>()
    );
    assert!(!mgr.is_pinyin(), "wubi86 不应判定为拼音");
}

#[test]
fn test_wubi_extra_dict_loaded() {
    let dir = data_dir();
    if !schema_exists(&dir, "wubi86") {
        eprintln!("跳过：wubi86 schema 不存在");
        return;
    }
    // 尝试删除**源旁**的旧 combined 缓存（扩展名此前还写着 .wdb，格式迁移后已是 .wdat）。
    //
    // 注意这行够不到真正的缓存：EngineManager::new 会把 CACHE_DIR 初始化成
    // %LOCALAPPDATA%\...\cache，cache_path 届时返回 <cache>/wubi86/wubi86_jidian.combined.wdat
    // （还会剥掉 .dict 中缀），本测试因此实际可能走缓存命中而非重新合并。
    // 核心断言（扩展库独有的词能查到）不依赖是否重合并，故不阻塞；要真正覆盖合并路径，
    // 需让 CACHE_DIR 可注入——它现在是进程级 OnceLock，测试无法重设。
    let _ = std::fs::remove_file(dir.join("schemas/wubi86/wubi86_jidian.dict.combined.wdat"));
    let cfg = make_config(&["wubi86"]);
    let mgr = EngineManager::new(&cfg, Some(&dir));

    // "甘蓝菜"(aaae) 仅存在于扩展库 wubi86_jidian_extra；主库没有。
    // 能查到即证明扩展库已被合并加载。
    let r = mgr.convert("aaae", 20);
    assert!(
        r.candidates.iter().any(|c| c.text == "甘蓝菜"),
        "扩展库词 '甘蓝菜'(aaae) 应能查到，实际: {:?}",
        r.candidates
            .iter()
            .take(10)
            .map(|c| c.text.as_str())
            .collect::<Vec<_>>()
    );

    // 主库词仍在
    let a = mgr.convert("aaaa", 20);
    assert!(
        a.candidates.iter().any(|c| c.text == "恭恭敬敬"),
        "主库词应仍在"
    );
}

/// 后台预热 + single-flight 构建锁：并发预热同一方案不重复构建/不死锁，最终就绪。
#[test]
fn test_prewarm_single_flight() {
    let dir = data_dir();
    if !schema_exists(&dir, "wubi86") || !schema_exists(&dir, "pinyin") {
        eprintln!("跳过：需要 wubi86 + pinyin schema");
        return;
    }
    let cfg = make_config(&["wubi86", "pinyin"]); // active=wubi86
    let mgr = std::sync::Arc::new(EngineManager::new(&cfg, Some(&dir)));

    assert!(mgr.is_loaded("wubi86"), "活跃方案应已同步加载");
    assert!(!mgr.is_loaded("pinyin"), "非活跃方案初始未加载");

    // 4 线程并发预热同一方案：single-flight 应只构建一次、不死锁、全部成功返回。
    let handles: Vec<_> = (0..4)
        .map(|_| {
            let m = std::sync::Arc::clone(&mgr);
            std::thread::spawn(move || m.prewarm_schema("pinyin"))
        })
        .collect();
    let oks: Vec<bool> = handles.into_iter().map(|h| h.join().unwrap()).collect();

    assert!(oks.iter().all(|&b| b), "并发预热应全部成功: {oks:?}");
    assert!(mgr.is_loaded("pinyin"), "预热后 pinyin 应已加载");
    assert!(!mgr.is_building("pinyin"), "加载完成后不应再报构建中");
}

/// 扩展词库 **live 热插拔**：对已加载引擎翻 enabled 标志即时改候选，无需重建。
#[test]
fn test_codetable_extra_hot_toggle() {
    let dir = data_dir();
    if !schema_exists(&dir, "wubi86") {
        eprintln!("跳过：wubi86 schema 不存在");
        return;
    }
    let cfg = make_config(&["wubi86"]);
    let mgr = EngineManager::new(&cfg, Some(&dir));

    // 扩展词库 id（非默认、有 path）
    let Some(merged) = mgr.schema_merged("wubi86") else {
        eprintln!("跳过：无法读取 wubi86");
        return;
    };
    let extra_ids: Vec<String> = merged
        .dictionaries
        .iter()
        .filter(|d| !d.default && !d.path.is_empty())
        .map(|d| d.id.clone())
        .collect();
    if extra_ids.is_empty() {
        eprintln!("跳过：wubi86 无扩展词库");
        return;
    }

    // 触发引擎加载并确认扩展库词 '甘蓝菜'(aaae) 初始可见
    let has_extra = |m: &EngineManager| {
        m.convert("aaae", 20)
            .candidates
            .iter()
            .any(|c| c.text == "甘蓝菜")
    };
    if !has_extra(&mgr) {
        eprintln!("跳过：扩展库词 '甘蓝菜' 不在该数据集");
        return;
    }

    // 热关闭全部扩展（live，不重建）→ '甘蓝菜' 消失
    for id in &extra_ids {
        assert!(
            mgr.set_dict_enabled_live("wubi86", id, false),
            "已加载引擎应即时命中扩展层: {id}"
        );
    }
    assert!(
        !has_extra(&mgr),
        "热关闭扩展后 '甘蓝菜' 应消失（live，未重建）"
    );
    assert!(
        mgr.convert("aaaa", 20)
            .candidates
            .iter()
            .any(|c| c.text == "恭恭敬敬"),
        "主库词不受扩展开关影响"
    );

    // 热重新开启 → '甘蓝菜' 回来
    for id in &extra_ids {
        assert!(mgr.set_dict_enabled_live("wubi86", id, true));
    }
    assert!(has_extra(&mgr), "热开启扩展后 '甘蓝菜' 应回来");
}

#[test]
fn test_pinyin_engine_candidates() {
    let dir = data_dir();
    if !schema_exists(&dir, "pinyin") {
        eprintln!("跳过：pinyin schema 不存在");
        return;
    }
    let cfg = make_config(&["pinyin"]);
    let mgr = EngineManager::new(&cfg, Some(&dir));

    assert!(mgr.is_pinyin(), "pinyin 应判定为拼音");

    let result = mgr.convert("nihao", 9);
    assert!(!result.candidates.is_empty(), "拼音 'nihao' 应产出候选");
    let top10: Vec<&str> = result
        .candidates
        .iter()
        .take(10)
        .map(|c| c.text.as_str())
        .collect();
    // 整句应在首位（等效词频量纲，旧为 SENTENCE_WEIGHT_BASE 置顶）。
    assert_eq!(
        result.candidates[0].text, "你好",
        "首候选应为 你好，实际: {top10:?}"
    );
    // 前缀子候选「你」应存在并标注只消费「ni」（分段上屏）。
    let ni = result.candidates.iter().find(|c| c.text == "你");
    assert!(ni.is_some(), "应包含前缀候选 你，实际: {top10:?}");
    assert_eq!(ni.unwrap().consumed_length, 2, "你 应只消费 ni 两字节");
    // 非前缀子串「好」（来自 hao 段）不应作为 nihao 的直接候选出现。
    assert!(
        !result.candidates.iter().any(|c| c.text == "好"),
        "不应包含非前缀子候选 好，实际: {top10:?}"
    );
}

#[test]
fn test_pinyin_long_sentence() {
    let dir = data_dir();
    if !schema_exists(&dir, "pinyin") {
        eprintln!("跳过：pinyin schema 不存在");
        return;
    }
    let cfg = make_config(&["pinyin"]);
    let mgr = EngineManager::new(&cfg, Some(&dir));

    // 长拼音串：Viterbi 整句解码应产出一个 >=4 字的合理句子候选
    let r = mgr.convert("woaizhongguo", 20);
    let longest = r
        .candidates
        .iter()
        .map(|c| c.text.chars().count())
        .max()
        .unwrap_or(0);
    eprintln!(
        "woaizhongguo 候选: {:?}",
        r.candidates
            .iter()
            .take(8)
            .map(|c| c.text.as_str())
            .collect::<Vec<_>>()
    );
    assert!(
        longest >= 4,
        "长句应产出 >=4 字候选（Viterbi+unigram），最长仅 {} 字",
        longest
    );
    assert!(
        r.candidates.iter().any(|c| c.text == "我爱中国"),
        "应能整句解码出 我爱中国"
    );
}

#[test]
fn test_mixed_wubi_priority_and_consistency() {
    let dir = data_dir();
    if !schema_exists(&dir, "wubi86_pinyin") {
        eprintln!("跳过：wubi86_pinyin schema 不存在");
        return;
    }
    let cfg = make_config(&["wubi86_pinyin"]);
    let mgr = EngineManager::new(&cfg, Some(&dir));

    // cang：五笔精确全码「駏」(+10M tier) 应压过拼音「藏」(/100)，首候选=駏（五笔优先）。
    let r = mgr.convert("cang", 9);
    let top: Vec<&str> = r
        .candidates
        .iter()
        .take(6)
        .map(|c| c.text.as_str())
        .collect();
    assert_eq!(
        r.candidates[0].text, "駏",
        "cang 首候选应为五笔精确码 駏，实际: {top:?}"
    );
    // 一致性：若放行全码自动上屏，commit_text 必等于显示首候选（杜绝显示/上屏漂移）。
    if r.should_commit {
        assert_eq!(
            r.commit_text, r.candidates[0].text,
            "全码上屏文本应与首候选一致"
        );
    }
    // 拼音「藏」仍在候选中（可选），只是不在首位。
    assert!(
        r.candidates.iter().any(|c| c.text == "藏"),
        "藏 应仍可选: {top:?}"
    );
}

#[test]
fn test_mixed_multisyllable_pinyin_preedit_separated() {
    let dir = data_dir();
    if !schema_exists(&dir, "wubi86_pinyin") {
        eprintln!("跳过：wubi86_pinyin schema 不存在");
        return;
    }
    let cfg = make_config(&["wubi86_pinyin"]);
    let mgr = EngineManager::new(&cfg, Some(&dir));
    // 多音节拼音：组合区应带音节分隔（"ni'hao"），而非连写。
    let r = mgr.convert("nihao", 9);
    assert!(
        r.preedit_display.contains('\''),
        "多音节拼音组合区应有音节分隔，实际 preedit: {:?}",
        r.preedit_display
    );
    // 混输高亮跟随：拼音拆分形态须单独留存（供协调器在高亮拼音候选时取用、高亮五笔候选时
    // 改回原始码）。多音节拼音应填充且含 ' 分隔。
    assert!(
        r.preedit_pinyin.contains('\''),
        "混输应留存拼音拆分形态 preedit_pinyin，实际: {:?}",
        r.preedit_pinyin
    );
}

#[test]
fn test_pinyin_trailing_partial_keeps_sentence() {
    let dir = data_dir();
    if !schema_exists(&dir, "pinyin") {
        eprintln!("跳过：pinyin schema 不存在");
        return;
    }
    let cfg = make_config(&["pinyin"]);
    let mgr = EngineManager::new(&cfg, Some(&dir));

    // 尾部多打一个不成音节的残码「m」：整句「你好」**不得被残码破坏**（bug①），
    // 残码不计入消费（consumed_length=5，留「m」在缓冲续输）。
    //
    // ⚠️ 本测试**两度**收窄过断言，两次都是因为它在测别的层的事：
    //
    // ① 原先断言「整句排首位」。那是 bug① 修复时顺手写下的实现细节，它真正要守的是
    //    「整句没被残码毁掉」——当年的故障形态是整句**消失/退化**，不是排序。
    // ② 后改断言「首选是你好吗」（step 6.5b 让位的结果）。step 2c 落地后这条也不成立了：
    //    Viterbi 现在**直接产出**「你好吗」作为残码整句（`prefix=0`、`code=nihaom`），
    //    它不再是「让位的受益者」而就是整句本身，6.5b 自然不触发（`is_sentence_demoted=0`）。
    //    此时引擎层首选回到消费更少但 log_prob 更高的「你好」（少一个词），**这没有错**：
    //    引擎侧刻意不按 `consumed_length` 排序（那会让消费少的候选被 truncate 整批丢弃，
    //    见 `handle_candidate.rs` 比较链 ⓪ 的注释）。
    //
    // ⇒ 「谁排首位」是**协调器**的事，由 `wind-coordinator/tests/pinyin_trailing_partial_order.rs`
    //    守着（那里断言 nihaom 首选是「你好吗」）。本测试只守引擎层的不变量：
    //    两个候选都在、身份正确、consumed_length 各自正确。
    let r = mgr.convert("nihaom", 9);
    let top: Vec<&str> = r
        .candidates
        .iter()
        .take(8)
        .map(|c| c.text.as_str())
        .collect();
    let responded = r
        .candidates
        .iter()
        .find(|c| c.text == "你好吗")
        .unwrap_or_else(|| panic!("「你好吗」须在候选中（它响应了残码 m），实际: {top:?}"));
    assert_eq!(
        responded.consumed_length, 6,
        "「你好吗」须消费全部 6 键，实际: {top:?}"
    );
    let sentence = r
        .candidates
        .iter()
        .find(|c| c.text == "你好")
        .unwrap_or_else(|| panic!("整句「你好」须仍在候选中（让位≠销毁），实际: {top:?}"));
    assert!(
        sentence.is_sentence,
        "「你好」须仍带整句身份（is_sentence 表来源、不因降级而清），实际: {top:?}"
    );
    assert_eq!(
        sentence.consumed_length, 5,
        "你好 应只消费 nihao 五字节，残码 m 留缓冲"
    );
}

#[test]
fn test_pinyin_bare_initial_prefers_single_char() {
    let dir = data_dir();
    if !schema_exists(&dir, "pinyin") {
        eprintln!("跳过：pinyin schema 不存在");
        return;
    }
    let cfg = make_config(&["pinyin"]);
    let mgr = EngineManager::new(&cfg, Some(&dir));

    // 裸声母 "m"（无完整音节，候选全为前缀补全词）：单字候选（吗/么）应优先于多字词
    // （没有/目前），对齐主流输入法首字优先。断言首候选为单字，且单字全部聚于多字词之前。
    let r = mgr.convert("m", 10);
    assert!(!r.candidates.is_empty(), "m 应产出候选");
    let top: Vec<&str> = r
        .candidates
        .iter()
        .take(10)
        .map(|c| c.text.as_str())
        .collect();
    assert_eq!(
        r.candidates[0].text.chars().count(),
        1,
        "裸声母 m 首候选应为单字，实际: {top:?}"
    );
    // 单字聚前：一旦出现多字词，其后不应再有单字（引擎层无 freq 重排，严格成立）。
    let mut seen_multi = false;
    for c in &r.candidates {
        if c.text.chars().count() > 1 {
            seen_multi = true;
        } else {
            assert!(
                !seen_multi,
                "单字 {:?} 出现在多字词之后，单字优先未生效: {top:?}",
                c.text
            );
        }
    }
}

#[test]
fn test_pinyin_trailing_partial_prefix_floats_above_exact() {
    let dir = data_dir();
    if !schema_exists(&dir, "pinyin") {
        eprintln!("跳过：pinyin schema 不存在");
        return;
    }
    let cfg = make_config(&["pinyin"]);
    let mgr = EngineManager::new(&cfg, Some(&dir));

    // meiy 尾部 "y" 未成音节：前缀补全「没有」应排在精确子串单字「没」之前。
    // 若标 is_prefix=true 会被协调器/引擎排序压到数百条 is_prefix=false 之后而不可见，
    // 修复（不标 is_prefix）使 prefix 补全经 is_partial=false 浮到 is_partial=true 之上。
    let r = mgr.convert("meiy", 400);
    let pos_meiyou = r.candidates.iter().position(|c| c.text == "没有");
    let pos_mei = r.candidates.iter().position(|c| c.text == "没");
    assert!(pos_meiyou.is_some(), "没有 应产出");
    assert!(pos_mei.is_some(), "没 应产出");
    let top: Vec<&str> = r
        .candidates
        .iter()
        .take(15)
        .map(|c| c.text.as_str())
        .collect();
    assert!(
        pos_meiyou.unwrap() < pos_mei.unwrap(),
        "前缀补全 没有 应在上层、排在精确子串 没 之前，实际前15: {top:?}"
    );
    // meiyou（无残码）保持现状：整句 没有 首位，前缀补全沉在精确匹配之后。
    let r_full = mgr.convert("meiyou", 400);
    let pos_prefix_after = r_full.candidates.iter().position(|c| c.is_prefix);
    let pos_exact_last = r_full.candidates.iter().rposition(|c| !c.is_prefix);
    if let (Some(pp), Some(pe)) = (pos_prefix_after, pos_exact_last) {
        assert!(pp > pe, "无残码时前缀补全应排在精确匹配之后");
    }
}

/// 长用户词上浮的端到端回归（真实拼音词库 + store 学习词）：
/// ① 全拼精确命中恒居首，且**不被错误整句挤下**（step6.5 降级配合）；
/// ② 打到第 3 个音节（含「整音节 + 残码」如 qingfengs）用户长词应上浮显现；
/// ③ 只打 2 音节（qingfeng，无残码）不上浮，精确整句仍居首。
/// weight=0 模拟「学习词」（词频另在协调器算），是最易触发上浮边界的场景。
#[test]
fn user_long_word_promotion_end_to_end() {
    let dir = data_dir();
    if !schema_exists(&dir, "pinyin") {
        eprintln!("跳过：pinyin schema 不存在");
        return;
    }
    let cfg = make_config(&["pinyin"]);
    let sp = std::env::temp_dir().join("wind_e2e_longword.redb");
    let _ = std::fs::remove_file(&sp);
    let store = std::sync::Arc::new(wind_store::Store::open(&sp).unwrap());
    store
        .add_user_word(
            "pinyin",
            "cangmangdetianyashiwodeai",
            "苍茫的天涯是我的爱",
            0,
            0,
        )
        .unwrap();
    store
        .add_user_word("pinyin", "qingfengshurufa", "清风输入法", 0, 0)
        .unwrap();
    let mgr = EngineManager::with_store(&cfg, Some(&dir), Some(store));

    // ① 全拼：学习词精确命中居首，不被合成整句挤下。
    let r = mgr.convert("cangmangdetianyashiwodeai", 400);
    assert_eq!(
        r.candidates.first().map(|c| c.text.as_str()),
        Some("苍茫的天涯是我的爱"),
        "全拼精确的学习词应居首，实际前3: {:?}",
        r.candidates
            .iter()
            .take(3)
            .map(|c| &c.text)
            .collect::<Vec<_>>()
    );

    // ② 整音节 + 残码（qingfengs）：用户长词应上浮（is_promoted_completion）。
    let r = mgr.convert("qingfengs", 400);
    let w = r
        .candidates
        .iter()
        .find(|c| c.text == "清风输入法")
        .expect("qingfengs 应出现清风输入法");
    assert!(w.is_promoted_completion, "qingfengs 下清风输入法应上浮");

    // 整 3 音节（qingfengshu）：同样上浮。
    let r = mgr.convert("qingfengshu", 400);
    assert!(
        r.candidates
            .iter()
            .any(|c| c.text == "清风输入法" && c.is_promoted_completion),
        "qingfengshu 下清风输入法应上浮"
    );

    // ③ 仅 2 音节无残码（qingfeng）：不上浮；精确整句「清风」居首。
    let r = mgr.convert("qingfeng", 400);
    assert_eq!(
        r.candidates.first().map(|c| c.text.as_str()),
        Some("清风"),
        "qingfeng 首选应是精确整句「清风」"
    );
    if let Some(w) = r.candidates.iter().find(|c| c.text == "清风输入法") {
        assert!(
            !w.is_promoted_completion,
            "qingfeng(2 音节无残码)不应上浮清风输入法"
        );
    }
}

#[test]
fn test_schema_cycle() {
    let dir = data_dir();
    if !schema_exists(&dir, "wubi86") || !schema_exists(&dir, "pinyin") {
        eprintln!("跳过：缺少 schema");
        return;
    }
    let cfg = make_config(&["wubi86", "pinyin"]);
    let mgr = EngineManager::new(&cfg, Some(&dir));

    assert_eq!(mgr.active_schema_id(), "wubi86");
    let next = mgr.cycle_schema();
    assert_eq!(next.as_deref(), Some("pinyin"));
    assert_eq!(mgr.active_schema_id(), "pinyin");
    assert!(mgr.is_pinyin());
}

/// available 中夹杂构建失败的方案时，循环应跳过它找到下一个已加载方案。
#[test]
fn test_schema_cycle_skips_unloaded() {
    let dir = data_dir();
    if !schema_exists(&dir, "wubi86") || !schema_exists(&dir, "pinyin") {
        eprintln!("跳过：缺少 schema");
        return;
    }
    // 中间插入一个不存在的方案 → 构建失败、不会进入 engines
    let cfg = make_config(&["wubi86", "__nonexistent__", "pinyin"]);
    let mgr = EngineManager::new(&cfg, Some(&dir));
    assert_eq!(mgr.active_schema_id(), "wubi86");
    let next = mgr.cycle_schema();
    assert_eq!(
        next.as_deref(),
        Some("pinyin"),
        "应跳过未加载方案直达 pinyin"
    );
}

/// 直达热键可切到**未启用**方案（不在 available 里），且之后循环键能切回来。
///
/// 英文方案的典型用法：不放进 available（省得占循环位），靠热键切过去打一段再切回。
/// 从这种状态循环时**不得跳过 available[0]**——原实现把「当前方案不在列表」当成
/// 「在第 0 个」，于是恰好漏掉第一个方案；那条兜底以前几乎不触发，能切到未启用方案
/// 之后就成了常规路径。
#[test]
fn test_switch_to_unavailable_schema_then_cycle_back() {
    let dir = data_dir();
    if !schema_exists(&dir, "wubi86") || !schema_exists(&dir, "pinyin") {
        eprintln!("跳过：缺少 schema");
        return;
    }
    // available 只有 wubi86，pinyin 未启用。
    let cfg = make_config(&["wubi86"]);
    let mgr = EngineManager::new(&cfg, Some(&dir));
    assert_eq!(mgr.active_schema_id(), "wubi86");

    // 切到未启用方案：懒加载，不看 available。
    assert!(
        mgr.switch_schema("pinyin"),
        "未启用方案也应能被直达热键切到"
    );
    assert_eq!(mgr.active_schema_id(), "pinyin");

    // 从未启用方案循环：available 只有 wubi86，应回到它而不是无处可去。
    let next = mgr.cycle_schema();
    assert_eq!(
        next.as_deref(),
        Some("wubi86"),
        "当前方案不在 available 时，循环应从 available[0] 起，不得跳过它"
    );
}

/// 方案显示名取自 schema.name（friendly），未知方案回退 id。
#[test]
fn test_schema_name_from_meta() {
    let dir = data_dir();
    if !schema_exists(&dir, "wubi86") || !schema_exists(&dir, "pinyin") {
        eprintln!("跳过：缺少 schema");
        return;
    }
    let cfg = make_config(&["wubi86"]);
    let mgr = EngineManager::new(&cfg, Some(&dir));
    assert_eq!(mgr.schema_name("wubi86"), "五笔");
    assert_eq!(mgr.schema_name("pinyin"), "全拼");
    // 未知方案：回退 id 本身
    assert_eq!(mgr.schema_name("__nonexistent__"), "__nonexistent__");
}

/// 配置热重载：切换活跃方案、更新可用列表，无需重建 EngineManager。
#[test]
fn test_reload_from_config_switches_active() {
    let dir = data_dir();
    if !schema_exists(&dir, "wubi86") || !schema_exists(&dir, "pinyin") {
        eprintln!("跳过：缺少 schema");
        return;
    }
    let cfg = make_config(&["wubi86", "pinyin"]);
    let mgr = EngineManager::new(&cfg, Some(&dir));
    assert_eq!(mgr.active_schema_id(), "wubi86");
    assert!(!mgr.is_pinyin());

    // 新配置：活跃切到 pinyin（顺序也变）
    let mut cfg2 = Config::default();
    cfg2.schema.available = vec!["pinyin".to_string(), "wubi86".to_string()];
    cfg2.schema.active = "pinyin".to_string();
    let changed = mgr.reload_from_config(&cfg2);
    assert!(changed, "活跃方案应从 wubi86 切到 pinyin");
    assert_eq!(mgr.active_schema_id(), "pinyin");
    assert!(mgr.is_pinyin(), "重载后应为拼音引擎");
    assert_eq!(
        mgr.available_schemas(),
        vec!["pinyin".to_string(), "wubi86".to_string()],
        "可用列表应反映新配置"
    );

    // 相同配置再次重载：活跃未变 → false
    let again = mgr.reload_from_config(&cfg2);
    assert!(!again, "活跃方案未变时应返回 false");
    assert_eq!(mgr.active_schema_id(), "pinyin");
}

/// 简拼（声母缩写）经 wdat 独立 AbbrevSection 产出候选：bzd→不知道 / bj→北京 等。
/// 这是「简拼能力」的回归保护（迁 wdat 前简拼完全失效，返回空）。
#[test]
fn test_pinyin_abbrev() {
    let dir = data_dir();
    if !schema_exists(&dir, "pinyin") {
        eprintln!("跳过：pinyin schema 不存在");
        return;
    }
    let cfg = make_config(&["pinyin"]);
    let mgr = EngineManager::new(&cfg, Some(&dir));
    let has = |input: &str, want: &str| -> bool {
        mgr.convert(input, 20)
            .candidates
            .iter()
            .any(|c| c.text == want)
    };
    assert!(has("bzd", "不知道"), "简拼 bzd 应含 不知道");
    assert!(has("bj", "北京"), "简拼 bj 应含 北京");
    assert!(has("nh", "你好"), "简拼 nh 应含 你好");
    assert!(has("zg", "中国"), "简拼 zg 应含 中国");
    assert!(has("zgr", "中国人"), "三字简拼 zgr 应含 中国人");
    // 全拼仍正常（简拼区段不影响全拼查询）。
    assert!(has("nihao", "你好"), "全拼 nihao 应含 你好");
}

/// 回归：整句「苍茫的天涯是我的爱」不被低频 3 字词「填鸭式」挤掉首选。
///
/// 根因＝unigram 独立性假设让 `P(天涯)·P(是)` 双重扣 `ln(total)`，加上每词罚 WORD_PENALTY，
/// 一个低频整词（填鸭式 w=152）便压过 2 词正解。修法＝虚词（是/的/了…）豁免每词罚
/// （见 `lattice::score_node`）。真正的解是 bigram，此为近似补偿。
#[test]
fn sentence_function_word_not_penalized_as_fragment() {
    let dir = data_dir();
    if !schema_exists(&dir, "pinyin") {
        eprintln!("跳过：pinyin schema 不存在");
        return;
    }
    let cfg = make_config(&["pinyin"]);
    let mgr = EngineManager::new(&cfg, Some(&dir));
    let r = mgr.convert("cangmangdetianyashiwodeai", 10);
    assert_eq!(
        r.candidates.first().map(|c| c.text.as_str()),
        Some("苍茫的天涯是我的爱"),
        "整句应以「天涯是」切分居首，不被「填鸭式」挤掉，实际前3: {:?}",
        r.candidates
            .iter()
            .take(3)
            .map(|c| &c.text)
            .collect::<Vec<_>>()
    );
}

// ───────────────────── 批量出码（纯词列表导入）─────────────────────
//
// 批量入口存在的唯一理由是把「读方案」提到循环外：`read_schema` 无缓存，每次调用都要
// 读盘 + 解析 TOML + 合并 override，逐词调 `encode_word` 在万级词表上会退化成万次文件
// 解析。既然是为性能分出的第二条路，就必须证明它与原路**结果完全一致**——否则单条加词
// 与批量导入会给同一个词出两种码，词库里留下打不出来的条目。

/// 批量与逐个必须逐位相同。这是批量入口的正当性所在：它只改准备时机，不改规则。
#[test]
fn encode_words_matches_encode_word_one_by_one() {
    let dir = data_dir();
    if !schema_exists(&dir, "wubi86") {
        eprintln!("跳过：wubi86 schema 不存在");
        return;
    }
    let mgr = EngineManager::new(&make_config(&["wubi86"]), Some(&dir));
    let words = ["中国", "计算机", "输入法", "人工智能", "王", "深度学习"];

    let batch = mgr.encode_words("wubi86", &words);
    let one_by_one: Vec<String> = words
        .iter()
        .map(|w| mgr.encode_word("wubi86", w).unwrap_or_default())
        .collect();

    assert_eq!(batch.len(), words.len(), "必须与入参同序等长");
    assert_eq!(
        batch, one_by_one,
        "批量与逐个出码必须逐位一致，否则加词与导入会给同一个词两种码"
    );
    assert!(
        batch.iter().any(|c| !c.is_empty()),
        "真实 wubi86 词库下不该整批都出不了码（说明 fixture 或取码链路坏了）"
    );
}

/// 单字必须**真的出得来码**，两个入口都是。
///
/// 上面那条等价性断言盖不住这个缺陷：词表里本就有单字「王」，而缺陷期两条路**都**返回
/// 空串——「一样错」同样满足「逐位一致」，测试照绿。根因是两个入口都把单字丢给
/// `calc_word_code`（词组公式，`chars.len() < 2` 即 `TooShort`），可单字要的码是它自己的
/// 全码。缺陷面是「给某个字补一条编码」这个加词界面上最常见的输入在码表方案下恒定失败。
///
/// 故这里断言的是**非空**，而不只是两边相等。
#[test]
fn single_char_encodes_from_both_entries() {
    let dir = data_dir();
    if !schema_exists(&dir, "wubi86") {
        eprintln!("跳过：wubi86 schema 不存在");
        return;
    }
    let mgr = EngineManager::new(&make_config(&["wubi86"]), Some(&dir));
    for ch in ["王", "工", "中"] {
        let one = mgr.encode_word("wubi86", ch);
        assert!(one.is_ok(), "单字「{ch}」应出得来码，实得 {one:?}");
        let one = one.unwrap_or_default();
        assert!(!one.is_empty(), "单字「{ch}」的码不该是空串");
        assert_eq!(
            mgr.encode_words("wubi86", &[ch]),
            vec![one],
            "单字「{ch}」在单条与批量两个入口下必须同码"
        );
    }
    // 码表里查不到的字仍须失败，且带上是哪个字——加词侧的排查线索全靠它。
    assert_eq!(
        mgr.encode_word("wubi86", "a"),
        Err(wind_engine::encoder::EncodeError::MissingCode { ch: 'a' }),
        "单字取不到码时应报 MissingCode 而非退化成 TooShort/空串"
    );
}

/// 出不了码的位置回**空串占位**，不是跳过——调用方靠下标把码配回词，
/// 少一个元素会让其后所有词错位配到别人的码上，静默写进词库。
#[test]
fn encode_words_keeps_position_for_unencodable() {
    let dir = data_dir();
    if !schema_exists(&dir, "wubi86") {
        eprintln!("跳过：wubi86 schema 不存在");
        return;
    }
    let mgr = EngineManager::new(&make_config(&["wubi86"]), Some(&dir));
    // 中间那个是拉丁字母，码表里必然取不到码。
    let words = ["中国", "abc", "输入法"];
    let codes = mgr.encode_words("wubi86", &words);

    assert_eq!(codes.len(), 3, "失败项必须占位，长度不能缩水");
    assert!(codes[1].is_empty(), "取不到码的位置应为空串");
    assert_eq!(
        codes[0],
        mgr.encode_word("wubi86", "中国").unwrap_or_default(),
        "前面的词不受影响"
    );
    assert_eq!(
        codes[2],
        mgr.encode_word("wubi86", "输入法").unwrap_or_default(),
        "失败项之后的词不能错位"
    );
}

#[test]
fn encode_words_handles_empty_input() {
    let dir = data_dir();
    let mgr = EngineManager::new(&make_config(&["wubi86"]), Some(&dir));
    assert!(mgr.encode_words("wubi86", &[]).is_empty());
    // 方案不存在时也要同序等长（全空串），不能 panic 或返回空 Vec。
    assert_eq!(
        mgr.encode_words("no_such_schema", &["中国", "计算机"])
            .len(),
        2
    );
}

/// 拼音侧同理：批量只把引擎句柄的获取提到循环外，产出必须与逐个一致。
#[test]
fn generate_words_pinyin_matches_one_by_one() {
    let dir = data_dir();
    if !schema_exists(&dir, "pinyin")
        || !dir.join("schemas/pinyin/cn_dicts/base.dict.yaml").exists()
    {
        eprintln!("跳过：pinyin schema/词库不存在");
        return;
    }
    let mgr = EngineManager::new(&make_config(&["pinyin"]), Some(&dir));
    let texts = ["中国", "计算机", "银行", "重复"];

    let batch = mgr.generate_words_pinyin("pinyin", &texts);
    let one_by_one: Vec<Option<String>> = texts
        .iter()
        .map(|t| mgr.generate_word_pinyin("pinyin", t))
        .collect();

    assert_eq!(batch.len(), texts.len(), "必须与入参同序等长");
    assert_eq!(batch, one_by_one, "批量与逐个生成必须逐位一致");
}

/// 批量入口的**存在理由**本身的对照：批量必须显著快于逐个。
///
/// 默认 `#[ignore]`（性能数字随机器波动，不适合当门禁），需要时手动跑：
/// `cargo test -p wind-engine --test engine_manager encode_words_batch_is_faster -- --ignored --nocapture`
///
/// 若有人日后把 `encode_words` 改回内部循环调用 `encode_word`，这个对照会立刻塌回 1x ——
/// 那正是本入口要防的退化（`read_schema` 无缓存，逐词调用 = 逐词读盘解析 TOML）。
#[test]
#[ignore]
fn encode_words_batch_is_faster_than_one_by_one() {
    let dir = data_dir();
    if !schema_exists(&dir, "wubi86") {
        eprintln!("跳过：wubi86 schema 不存在");
        return;
    }
    let mgr = EngineManager::new(&make_config(&["wubi86"]), Some(&dir));
    let base = ["中国", "计算机", "输入法", "人工智能", "深度学习"];
    let words: Vec<&str> = base.iter().cycle().take(1000).copied().collect();

    // 预热：首次调用要建单字全码表（有缓存），不让它计入任一侧。
    let _ = mgr.encode_words("wubi86", &base);

    let t0 = std::time::Instant::now();
    let batch = mgr.encode_words("wubi86", &words);
    let batch_ms = t0.elapsed();

    let t1 = std::time::Instant::now();
    let one_by_one: Vec<String> = words
        .iter()
        .map(|w| mgr.encode_word("wubi86", w).unwrap_or_default())
        .collect();
    let loop_ms = t1.elapsed();

    assert_eq!(batch, one_by_one, "快也必须给出相同结果");
    println!(
        "1000 词：批量 {:?} / 逐个 {:?} → {:.1}x",
        batch_ms,
        loop_ms,
        loop_ms.as_secs_f64() / batch_ms.as_secs_f64().max(1e-9)
    );
    assert!(
        batch_ms * 5 < loop_ms,
        "批量应至少快 5 倍（实测 批量 {batch_ms:?} vs 逐个 {loop_ms:?}）——\
         若接近 1x，多半是批量实现退化成了内部逐词调用 encode_word"
    );
}
