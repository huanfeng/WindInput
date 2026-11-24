//! 求值型候选（`date`/`time` 等短语）的 shadow 接线：规则须按**稳定 id** 落键与匹配，
//! 否则次日文本一变即失配——用户侧表现为「候选调整昨天设了、今天被还原」。
use super::*;
use std::sync::Arc;
use wind_config::config::Config;
use wind_store::store::Store;

fn store_at(name: &str) -> Arc<Store> {
    let p = std::env::temp_dir().join(name);
    let _ = std::fs::remove_file(&p);
    Arc::new(Store::open(&p).unwrap())
}

fn coord_with(store: Arc<Store>) -> Arc<Coordinator> {
    Coordinator::new_headless_with_store(Config::default(), None, store)
}

/// 一条纯文本候选（拼音/码表静态候选，无稳定 id → 规则按 word 匹配）。
fn plain(text: &str) -> Candidate {
    Candidate {
        text: text.to_string(),
        ..Default::default()
    }
}

/// 一条 `date` 短语候选：`text` 是当日求值结果，`id` 是模板身份。
fn date_cand(text: &str, template: &str) -> Candidate {
    Candidate {
        text: text.to_string(),
        is_phrase: true,
        is_exact_code: true,
        phrase_template: template.to_string(),
        id: Coordinator::phrase_cand_id("date", template),
        source: CandidateSource::Phrase,
        ..Default::default()
    }
}

/// id 构造：模板原文入 id，空模板 → 空 id（无稳定身份，落回文本匹配）。
#[test]
fn phrase_cand_id_shape() {
    assert_eq!(
        Coordinator::phrase_cand_id("date", "$Y-$MM-$DD"),
        "phrase:date:$Y-$MM-$DD"
    );
    assert_eq!(Coordinator::phrase_cand_id("date", ""), "");
}

/// 核心回归：昨天置顶的 `date` 格式，今天（文本已全变）依然置顶。
///
/// ⚠ 判别力全在「写规则用的文本」与「今天候选的文本」**不相同**——若两者相同，
/// 按 text 匹配的旧实现也会全绿，这个测试就成了假测试。
#[test]
fn pinned_date_format_survives_next_day() {
    let store = store_at("wind_coord_shadow_date.redb");
    let c = coord_with(store.clone());
    let schema = c
        .engine_mgr
        .data_schema_id(&c.engine_mgr.active_schema_id());
    let tpl = "$Y-$MM-$DD";

    // 昨天：用户把 `$Y-$MM-$DD` 那条置顶，规则 word = 昨天的求值文本。
    store
        .pin_shadow(
            &schema,
            "date",
            "2026-07-28",
            Some(&Coordinator::phrase_cand_id("date", tpl)),
            0,
        )
        .unwrap();

    // 今天：候选文本全变，且被置顶的那条排在第二位。
    let mut cands = vec![
        date_cand("2026年7月29日", "$Y年$M月$D日"),
        date_cand("2026-07-29", tpl),
        date_cand("2026.07.29", "$Y.$MM.$DD"),
    ];
    c.apply_shadow_in(None, &mut cands, "date");

    assert_eq!(
        cands.iter().map(|c| c.text.as_str()).collect::<Vec<_>>(),
        ["2026-07-29", "2026年7月29日", "2026.07.29"],
        "按 cand_id 匹配，昨天的置顶今天仍生效"
    );
}

/// 反向：规则不带 id（存量规则 / 手工添加）时仍按文本匹配，老行为不回归。
#[test]
fn legacy_rule_without_id_still_matches_by_text() {
    let store = store_at("wind_coord_shadow_legacy.redb");
    let c = coord_with(store.clone());
    let schema = c
        .engine_mgr
        .data_schema_id(&c.engine_mgr.active_schema_id());
    store.pin_shadow(&schema, "aaaa", "敬", None, 0).unwrap();

    let mut cands = vec![
        Candidate {
            text: "工".into(),
            ..Default::default()
        },
        Candidate {
            text: "敬".into(),
            ..Default::default()
        },
    ];
    c.apply_shadow_in(None, &mut cands, "aaaa");
    assert_eq!(
        cands.iter().map(|c| c.text.as_str()).collect::<Vec<_>>(),
        ["敬", "工"]
    );
}

/// **双拼击键读得到全拼下写的规则**——这是「拼音候选置顶」在双拼/全拼共用词库时
/// 能成立的全部理由。
///
/// `data_schema_id` 早已把两种方案折叠成同一个 schema，缺的一直是 code 维度：读写两端
/// 都取 `input_buffer`（击键域），于是双拼的 `hc` 与全拼的 `hao` 落成两个互不相认的键。
///
/// 反向对照不可省：末尾那段用击键码再读一次，**必须读不到**。没有它，这个测试在
/// 「归一码根本没接线、而 apply_shadow 恰好对空列表无操作」之类的情形下也会绿。
#[test]
fn shuangpin_keystroke_reads_rule_written_under_full_pinyin() {
    let store = store_at("wind_coord_shadow_norm.redb");
    let c = coord_with(store.clone());
    let schema = c
        .engine_mgr
        .data_schema_id(&c.engine_mgr.active_schema_id());

    // 全拼下把「好」置顶（或等价地：另一台机器/另一次会话用全拼写下的存量规则）。
    store.pin_shadow(&schema, "hao", "好", None, 0).unwrap();

    // 双拼击键 `hc`，引擎给出归一码 `hao`（见 ConvertResult::shadow_code）。
    let code = {
        let mut state = c.state.lock().unwrap_or_else(|e| e.into_inner());
        state.input_buffer = "hc".into();
        state.shadow_code = "hao".into();
        Coordinator::shadow_code_of(&state).to_string()
    };
    assert_eq!(code, "hao", "归一码非空时必须优先于击键缓冲");

    let mut cands = vec![plain("耗"), plain("好"), plain("号")];
    c.apply_shadow_in(None, &mut cands, &code);
    assert_eq!(
        cands.iter().map(|c| c.text.as_str()).collect::<Vec<_>>(),
        ["好", "耗", "号"],
        "双拼 hc 应读到全拼 hao 下的置顶规则"
    );

    // 反向对照：拿击键码去读，规则不该命中——证明上面的绿来自归一，而非别的原因。
    let mut raw = vec![plain("耗"), plain("好"), plain("号")];
    c.apply_shadow_in(None, &mut raw, "hc");
    assert_eq!(
        raw.iter().map(|c| c.text.as_str()).collect::<Vec<_>>(),
        ["耗", "好", "号"],
        "击键码 hc 下没有规则，顺序必须原样——否则上面那条断言证明不了归一起了作用"
    );
}

/// 全拼路径**恒等**：归一码为空时落回击键缓冲，存量规则一条都不用迁。
#[test]
fn full_pinyin_shadow_code_is_identity() {
    let c = coord_with(store_at("wind_coord_shadow_identity.redb"));
    let state = c.state.lock().unwrap_or_else(|e| e.into_inner());
    // 默认 State：input_buffer 与 shadow_code 均空。
    assert_eq!(Coordinator::shadow_code_of(&state), "");
    drop(state);

    let mut state = c.state.lock().unwrap_or_else(|e| e.into_inner());
    state.input_buffer = "xi'an".into();
    state.shadow_code.clear(); // 全拼引擎恒给空串
    assert_eq!(
        Coordinator::shadow_code_of(&state),
        "xi'an",
        "全拼须原样取击键（含手动分隔符——`'` 是硬边界，剥掉会与 xian 撞 key）"
    );
}

/// 短语上屏不写 FREQ：求值型文本每次都是新键，记了永不命中、只堆垃圾。
/// 同一条 code 下的码表候选照常记账，证明拦的是来源而非整条路径。
#[test]
fn phrase_commit_does_not_record_freq() {
    let store = store_at("wind_coord_phrase_freq.redb");
    let c = coord_with(store.clone());
    let schema = c
        .engine_mgr
        .data_schema_id(&c.engine_mgr.active_schema_id());

    c.record_selection("date", "2026-07-29", CandidateSource::Phrase);
    assert!(
        store
            .get_freq(&schema, "date", "2026-07-29")
            .unwrap()
            .is_none(),
        "短语候选不得写入词频"
    );

    // 上屏历史仍应记（`z` 键重复上屏 / cmdbar last() 依赖它）。
    assert_eq!(
        c.recent_commits_snapshot().first().map(|s| s.as_str()),
        Some("2026-07-29"),
        "不记词频不等于不记上屏历史"
    );
}
