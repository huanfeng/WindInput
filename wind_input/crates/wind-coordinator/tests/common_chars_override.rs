//! 常用字表的**用户覆盖**（候选右键「设为生僻字 / 设为常用字」）端到端。
//!
//! 装置沿用 `codetable_filter_scope_consistency.rs` 的现场：五笔 `sivg` 码位上坐着
//! 常用的「档」与生僻的「桜」，智能档只放行前者。把「档」标成生僻之后，这个码位就
//! **没有常用字了**，于是「桜」按孤儿码规则被放出来——一条断言同时验到了
//! 写库、镜像回灌、候选重建、过滤联动四步。
//!
//! ⚠️ `build_dev/data` 不存在时**整族静默跳过而计数照绿**（判据是耗时：正常秒级，
//! 跳过是 0.0x s）。恢复命令 `.\scripts\dev.ps1 gd`。

use std::path::PathBuf;
use std::sync::Arc;
use wind_bridge::handler::{KeyEventData, MessageHandler};
use wind_config::Config;
use wind_coordinator::Coordinator;
use wind_ipc::protocol::EVENT_KEY_DOWN;
use wind_ui_types::CandidateOp;

/// PageDown，默认翻页键组 "pageupdown"（末页再按一次即触发检索范围临时放宽）。
const VK_NEXT: u32 = 0x22;

fn data_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../build_dev/data")
}

fn dict_ready(d: &std::path::Path) -> bool {
    d.join("schemas/wubi86/wubi86_jidian.dict.yaml").exists()
}

fn key_event(key_code: u32) -> KeyEventData {
    KeyEventData {
        key_code,
        scan_code: 0,
        modifiers: 0,
        event_type: EVENT_KEY_DOWN,
        toggles: 0,
        event_seq: 0,
        prev_char: 0,
    }
}

/// 每个测试独立库：redb 单写者，共用文件会让并发测试互相阻塞。
fn store(tag: &str) -> Arc<wind_store::Store> {
    let p = std::env::temp_dir().join(format!(
        "wind_common_override_{}_{}.redb",
        std::process::id(),
        tag
    ));
    let _ = std::fs::remove_file(&p);
    Arc::new(wind_store::Store::open(&p).unwrap())
}

fn coord(tag: &str) -> Arc<Coordinator> {
    let mut cfg = Config::default();
    cfg.schema.available = vec!["wubi86".into()];
    cfg.schema.active = "wubi86".into();
    cfg.input.default.chinese_mode = true;
    cfg.input.filter_mode = "smart".into();
    Coordinator::new_headless_with_store(cfg, Some(&data_dir()), store(tag))
}

/// 按键走**生产入口** `handle_key_event_policed`，不是内部的 `handle_key_event`——
/// 后者绕过若干收口，等于验证一条真实不存在的路径。
fn press(coord: &Coordinator, code: &str) {
    for c in code.chars() {
        coord.handle_key_event_policed(&key_event((c.to_ascii_uppercase() as u32) & 0xFF));
    }
}

fn clear(coord: &Coordinator) {
    for _ in 0..12 {
        coord.handle_key_event_policed(&key_event(0x08)); // Backspace
    }
}

fn cands(coord: &Coordinator) -> Vec<String> {
    coord.debug_all_candidate_texts()
}

/// 打一串码并取候选。
fn cands_of(coord: &Coordinator, code: &str) -> Vec<String> {
    clear(coord);
    press(coord, code);
    cands(coord)
}

/// 页内第 n 项的位置（用于对着某条候选点右键）。
fn index_of(list: &[String], text: &str) -> Option<usize> {
    list.iter().position(|t| t == text)
}

/// ★ 核心链路：把一个字标成生僻，它**当场从候选里消失**，被它压着的生僻字露出来。
///
/// 走完整条路：写 redb → 回灌内存镜像 → 重建候选 → 智能过滤重算。任何一步断掉，
/// 屏幕上就毫无变化——那正是用户报的「设了没反应」。
///
/// ⚠️ 这里钉的是**用户显式降级不吃「孤儿码位」保底**（`Candidate::user_rare`）。
/// 不加那一位的话：「档」降级后 sivg 组变成「没有常用字」，保底把它原样放回、还在
/// 第一位——智能档因此成了三档里唯一「设了看不出」的一档。
#[test]
fn marking_a_char_rare_removes_it_from_candidates() {
    let d = data_dir();
    if !dict_ready(&d) {
        eprintln!("跳过：build_dev/data 缺失");
        return;
    }
    let c = coord("release");

    let before = cands_of(&c, "sivg");
    assert!(
        before.contains(&"档".to_string()),
        "前置不成立：sivg 应有常用字「档」，实际 {before:?}"
    );
    assert!(
        !before.contains(&"桜".to_string()),
        "前置不成立：智能档本应压住生僻的「桜」，实际 {before:?}"
    );

    // 对着「档」点右键 →「设为生僻字」。
    let idx = index_of(&before, "档").expect("「档」应在候选里");
    assert_eq!(
        c.debug_common_char_mark(idx),
        Some(("档".to_string(), true)),
        "菜单侧应认「档」为当前判常用（据此给「设为生僻字」）"
    );
    c.debug_candidate_op(CandidateOp::ToggleCommon, idx);

    let after = cands(&c);
    assert!(
        !after.contains(&"档".to_string()),
        "降级的字应当场消失，实际 {after:?}"
    );
    assert!(
        after.contains(&"桜".to_string()),
        "「档」让开后，本被它压着的「桜」应露出来，实际 {after:?}"
    );
}

/// 滤掉**不等于**打不出：末页再按一次翻页键，放宽就能把它调回来。
///
/// 这是「直接滤掉」这个设计成立的前提（2026-08-24 用户拍板时点明的那条出路）。
/// 缺了它，把某码位唯一的字降级就等于把那个字永久锁死——而用户当时只是想让它别挡路。
#[test]
fn demoted_char_comes_back_via_scope_relax() {
    let d = data_dir();
    if !dict_ready(&d) {
        eprintln!("跳过：build_dev/data 缺失");
        return;
    }
    let c = coord("relax");

    let before = cands_of(&c, "sivg");
    let idx = index_of(&before, "档").expect("sivg 应有「档」");
    c.debug_candidate_op(CandidateOp::ToggleCommon, idx);
    assert!(!cands(&c).contains(&"档".to_string()), "先确认它已被滤掉");

    // 末页再按一次向后翻页键 → 临时放宽，被滤的候选追加到末尾。
    c.handle_key_event_policed(&key_event(VK_NEXT));
    let relaxed = cands(&c);
    assert!(
        relaxed.contains(&"档".to_string()),
        "放宽后应能把降级的字调回来，实际 {relaxed:?}"
    );
    assert_eq!(
        relaxed.last().map(String::as_str),
        Some("档"),
        "放宽的候选追加在**末尾**，原有顺序纹丝不动，实际 {relaxed:?}"
    );
}

/// 覆盖是**全局**的：换一个码位打同一个字，判定跟着走。
///
/// 这正是它与 shadow 的分界——shadow 键含输入码，只在那个码下生效。
#[test]
fn override_applies_across_codes() {
    let d = data_dir();
    if !dict_ready(&d) {
        eprintln!("跳过：build_dev/data 缺失");
        return;
    }
    let c = coord("global");

    // 「档」的简码 siv 与全码 sivg 是两个不同的输入码。
    let full = cands_of(&c, "sivg");
    let idx = index_of(&full, "档").expect("sivg 应有「档」");
    c.debug_candidate_op(CandidateOp::ToggleCommon, idx);

    // 换到简码 siv 上再看：同一个字，判定跟着走 ⇒ 这里也该被滤掉。
    let short = cands_of(&c, "siv");
    assert!(
        !short.contains(&"档".to_string()),
        "覆盖不带输入码，换个码打同一个字也该降级（这正是它与 shadow 的分界），实际 {short:?}"
    );
    // 放宽后仍能调出来——降级是全局的，出路也是全局的。
    c.handle_key_event_policed(&key_event(VK_NEXT));
    assert!(cands(&c).contains(&"档".to_string()));
}

/// 点回出厂方向 = **删掉覆盖**，而不是写一条同向记录。
///
/// 库里因此永远只留「与出厂不同」的字：词库管理界面列出来的就是一份干净的
/// 「我改过的」，出厂表升版时没被碰过的字自动跟随。
#[test]
fn toggling_back_removes_the_row_instead_of_writing_a_redundant_one() {
    let d = data_dir();
    if !dict_ready(&d) {
        eprintln!("跳过：build_dev/data 缺失");
        return;
    }
    let st = store("roundtrip");
    let mut cfg = Config::default();
    cfg.schema.available = vec!["wubi86".into()];
    cfg.schema.active = "wubi86".into();
    cfg.input.default.chinese_mode = true;
    cfg.input.filter_mode = "smart".into();
    let c = Coordinator::new_headless_with_store(cfg, Some(&d), Arc::clone(&st));

    let list = cands_of(&c, "sivg");
    let idx = index_of(&list, "档").expect("sivg 应有「档」");

    c.debug_candidate_op(CandidateOp::ToggleCommon, idx);
    assert_eq!(
        st.get_common_char_override("档").unwrap(),
        Some(false),
        "第一次点击应写下一条与出厂相反的覆盖"
    );

    // 再切回来。降级后它已从候选里消失，右键点不到——**得先放宽把它调出来**，
    // 这正是「滤掉但留一条出路」那个设计在用户手上的实际走法。
    c.handle_key_event_policed(&key_event(VK_NEXT));
    let list2 = cands(&c);
    let idx2 = index_of(&list2, "档").expect("放宽后「档」应回到列表末尾");
    c.debug_candidate_op(CandidateOp::ToggleCommon, idx2);
    assert_eq!(
        st.get_common_char_override("档").unwrap(),
        None,
        "点回出厂方向应删掉那条记录，而不是写一条同向的冗余覆盖"
    );
    assert!(
        st.list_common_char_overrides().unwrap().is_empty(),
        "库里不该留下任何痕迹"
    );

    // 行为也回到原样：生僻的「桜」重新被压住。
    let back = cands_of(&c, "sivg");
    assert!(
        !back.contains(&"桜".to_string()),
        "恢复出厂后应回到智能档的原始表现，实际 {back:?}"
    );
}

/// 词组不给标记：「常用」是**字**级属性，给词组存覆盖，读端逐字判定时永远看不到它。
#[test]
fn phrases_are_not_markable() {
    let d = data_dir();
    if !dict_ready(&d) {
        eprintln!("跳过：build_dev/data 缺失");
        return;
    }
    let c = coord("phrase");

    // ggg = 五笔「王王王」类多字词区；找一条真正的多字候选来断言。
    let list = cands_of(&c, "wgg");
    let multi = list.iter().position(|t| t.chars().count() > 1);
    if let Some(i) = multi {
        assert_eq!(
            c.debug_common_char_mark(i),
            None,
            "多字候选 {:?} 不该给标记项",
            list[i]
        );
    }
    // 单字候选则必须给。
    let single = list
        .iter()
        .position(|t| t.chars().count() == 1)
        .expect("wgg 应有单字候选");
    assert!(
        c.debug_common_char_mark(single).is_some(),
        "单字候选 {:?} 应可标记",
        list[single]
    );
}

/// 词库管理列表：全表 + 搜索 + 「只看已修改」。
///
/// 这三条必须在**有真实字表**的地方测。webdata 那侧的装置没有 data_dir，默认表为空 ⇒
/// 「全表」与「只看已修改」两个口径恰好等价，`only_modified` 传不传都一样，测了等于没测。
#[test]
fn list_covers_whole_table_and_filters_modified_only() {
    let d = data_dir();
    if !dict_ready(&d) {
        eprintln!("跳过：build_dev/data 缺失");
        return;
    }
    use wind_coordinator::web_host::WebDataHost;
    let c = coord("listing");

    // 全表：8104 字量级，远多于「改过的」。
    let all = c.common_char_rows("", false);
    assert!(
        all.len() > 8000,
        "应列出整张默认字表，实际 {} 行",
        all.len()
    );
    assert!(
        all.iter().all(|r| !r.overridden),
        "还没改过任何字，不该有行被标成已修改"
    );
    // 字表原序：一级字打头（`common_chars.txt` 按级别拼接）。
    assert_eq!(
        all[0].text, "一",
        "全表须按字表原序，不能是 HashSet 的随机序"
    );

    // 搜索：只留出现在查询串里的字。
    let hit = c.common_char_rows("的", false);
    assert_eq!(hit.len(), 1);
    assert_eq!(hit[0].text, "的");
    assert!(hit[0].common && hit[0].base_common, "「的」默认就是常用字");

    // 只看已修改：改之前空，改之后只剩那一条。
    assert!(c.common_char_rows("", true).is_empty());
    // 走设置页那条写端（`common_char_edit`），与界面点按钮时同一条路径。
    c.common_char_edit(
        "的",
        wind_coordinator::handle_common_chars::CommonCharEdit::Set(false),
    )
    .unwrap();
    let modified = c.common_char_rows("", true);
    assert_eq!(modified.len(), 1, "只该剩改过的那一条");
    assert_eq!(modified[0].text, "的");
    assert!(modified[0].overridden);
    assert!(
        !modified[0].common && modified[0].base_common,
        "默认常用、现在生僻——两个值都要在，界面靠差异显示对照"
    );
    // 全表口径下它仍在，只是判定变了（不是从表里消失）。
    assert!(c.common_char_rows("", false).len() > 8000);
}

/// 覆盖在**重启后**仍然生效——装载走 `build`，而不是只在 `new()` 里。
///
/// 这条钉的是回灌落点：若装载写在 `new()` 里，`new_headless_with_store` 这条路
/// （直接走 build）就恒看不到已存在的覆盖，而那正是本测试模拟的「重启」。
#[test]
fn overrides_survive_restart() {
    let d = data_dir();
    if !dict_ready(&d) {
        eprintln!("跳过：build_dev/data 缺失");
        return;
    }
    let st = store("restart");
    st.set_common_char_override("档", false).unwrap();

    let mut cfg = Config::default();
    cfg.schema.available = vec!["wubi86".into()];
    cfg.schema.active = "wubi86".into();
    cfg.input.default.chinese_mode = true;
    cfg.input.filter_mode = "smart".into();
    // 全新协调器，模拟重启：覆盖已在库里，必须在构造期被装载。
    let c = Coordinator::new_headless_with_store(cfg, Some(&d), Arc::clone(&st));

    let list = cands_of(&c, "sivg");
    assert!(
        list.contains(&"桜".to_string()),
        "重启后覆盖应照旧生效（「档」判生僻 ⇒ 放行「桜」），实际 {list:?}"
    );
}
