//! 码表逆切分的端到端验证：从按键入口走到候选窗显示与上屏。
//!
//! 引擎单测（`codetable/engine.rs` 的 `split_*`）证明的是组合候选的构造；本文件证明的是
//! 两件只有走完协调器才看得见的事：
//!
//! - **段候选吃候选调整**（论坛 t231）：`EngineManager` 真的把 store 注入了引擎。用户在
//!   段码上删掉的词不得被拼进组合——引擎单测里是手工 `with_segment_shadow`，漏接线照样绿；
//! - **次选只显示后段**（论坛 t231 / t11）：显示与上屏在协调器出口分叉，首条组合由
//!   显示出口 `cand_display_text` 判定为整串；
//! - **段内置顶**活过协调器的重排（引擎单测只看得到引擎序，审查实测曾被按权重排回）。
//!
//! 现场 `aaab`：五笔 86 里是空码，`aa`（式/戒…）与 `ab`（节/蒸…）都有重码。
//! 逆切分出厂关，经方案级 override 打开。
//!
//! ⚠️ 依赖 `build_dev/data` 真实词库；缺失时**静默跳过**（判据是耗时 0.00s）。

use std::path::PathBuf;
use std::sync::Arc;
use wind_bridge::handler::{KeyAction, KeyEventData, MessageHandler};
use wind_config::Config;
use wind_coordinator::Coordinator;
use wind_ipc::protocol::EVENT_KEY_DOWN;
use wind_store::Store;

fn data_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../build_dev/data")
}

fn has_data() -> bool {
    data_dir()
        .join("schemas/wubi86/wubi86_jidian.dict.yaml")
        .exists()
}

fn key(k: u32) -> KeyEventData {
    KeyEventData {
        key_code: k,
        scan_code: 0,
        modifiers: 0,
        event_type: EVENT_KEY_DOWN,
        toggles: 0,
        event_seq: 0,
        prev_char: 0,
    }
}

fn press(coord: &Coordinator, code: &str) {
    for c in code.chars() {
        coord.handle_key_event(&key(u32::from(c.to_ascii_uppercase())));
    }
}

/// 用例结束时删掉临时目录（Linux 上删已打开的 redb 文件无妨）。
struct TempRoot(PathBuf);
impl Drop for TempRoot {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// 每个用例独立的 store 与 override 目录（并发写同一文件会撕裂）；目录名带 pid，
/// 并发会话 / 并行的两次 `cargo test` 共用 temp 时不互相删库。
/// `extra` 追加到 `[engine.codetable]` 段。
fn coord_with(tag: &str, extra: &str) -> (Arc<Coordinator>, Arc<Store>, TempRoot) {
    let root = std::env::temp_dir().join(format!("wind_split_e2e_{tag}_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let ov = root.join("overrides");
    std::fs::create_dir_all(&ov).unwrap();
    std::fs::write(
        ov.join("wubi86.toml"),
        format!("[engine.codetable]\nsplit_input = true\n{extra}"),
    )
    .unwrap();
    let store = Arc::new(Store::open(root.join("user_data.db")).unwrap());
    let mut c = Config::default();
    c.schema.available = vec!["wubi86".into()];
    c.schema.active = "wubi86".into();
    c.input.default.chinese_mode = true;
    c.keys.select_char_keys = vec!["comma_period".into()];
    let coord = Coordinator::new_headless_with_store_override(
        c,
        Some(&data_dir()),
        store.clone(),
        Some(ov),
    );
    (coord, store, TempRoot(root))
}

const INPUT: &str = "aaab";

#[test]
fn alt_candidates_show_back_segment_but_commit_whole() {
    if !has_data() {
        eprintln!("跳过：缺 build_dev 词库");
        return;
    }
    let (coord, _store, _dir) = coord_with("alt", "");
    press(&coord, INPUT);
    let texts = coord.debug_page_texts();
    let shown = coord.debug_page_display_texts();
    assert!(
        texts.len() >= 2,
        "前提：`ab` 有重码，应有 ≥2 条组合，实际 {texts:?}"
    );
    assert_eq!(shown[0], texts[0], "首选（组合区）显示整串");
    assert!(
        shown[1] != texts[1] && texts[1].ends_with(shown[1].as_str()),
        "次选只显示后段：显示 {:?}，整串 {:?}",
        shown[1],
        texts[1]
    );
    // 选 ② 上屏的是**整串**，不是显示的那半截。
    match coord.handle_key_event(&key(u32::from(b'2'))) {
        KeyAction::InsertText { text, .. } => assert_eq!(text, texts[1]),
        other => panic!("选 ② 应上屏整串，实际 {other:?}"),
    }
}

/// 反向对照：`split_alt_display = "full"` ⇒ 显示与上屏同形（证明分叉来自这个开关）。
#[test]
fn full_display_keeps_whole_text() {
    if !has_data() {
        eprintln!("跳过：缺 build_dev 词库");
        return;
    }
    let (coord, _store, _dir) = coord_with("full", "split_alt_display = \"full\"\n");
    press(&coord, INPUT);
    let texts = coord.debug_page_texts();
    assert!(texts.len() >= 2, "前提：应有 ≥2 条组合，实际 {texts:?}");
    assert_eq!(coord.debug_page_display_texts(), texts);
    // ★ 前提对照：同一现场在默认档下**确实**分叉——否则 `aaab` 哪天不再走切分（词库加了
    // 条目），本条会因为「根本没有组合候选」而假绿。
    let (back, _store2, _dir2) = coord_with("full_ctl", "");
    press(&back, INPUT);
    assert_ne!(
        back.debug_page_display_texts()[1],
        back.debug_page_texts()[1]
    );
}

/// 论坛 t231：段码上被删除的词不再参与组合。
#[test]
fn deleted_segment_word_is_not_composed() {
    if !has_data() {
        eprintln!("跳过：缺 build_dev 词库");
        return;
    }
    let (coord, store, _dir) = coord_with("del", "");
    press(&coord, INPUT);
    let before = coord.debug_page_texts();
    let shown = coord.debug_page_display_texts();
    assert!(before.len() >= 2, "前提：应有 ≥2 条组合，实际 {before:?}");
    // 删掉后段 `ab` 的次选（即 ② 显示的那个词）。
    let victim = shown[1].clone();
    store.delete_shadow("wubi86", "ab", &victim).unwrap();

    coord.handle_key_event(&key(0x1B)); // Esc 清缓冲
    press(&coord, INPUT);
    let after = coord.debug_page_texts();
    assert!(
        !after.iter().any(|t| t == &before[1]),
        "`ab` 上删掉的「{victim}」仍被拼进组合：{after:?}"
    );
    assert_eq!(after.first(), before.first(), "首选不受影响");
}

/// 段内置顶要活过协调器的重排：在 `ab` 上把次选置顶，组合首选随之换成它。
#[test]
fn pinned_segment_word_leads_composition() {
    if !has_data() {
        eprintln!("跳过：缺 build_dev 词库");
        return;
    }
    let (coord, store, _dir) = coord_with("pin", "");
    press(&coord, INPUT);
    let before = coord.debug_page_texts();
    let shown = coord.debug_page_display_texts();
    assert!(before.len() >= 2, "前提：应有 ≥2 条组合，实际 {before:?}");
    store
        .pin_shadow("wubi86", "ab", &shown[1], None, 0)
        .unwrap();

    coord.handle_key_event(&key(0x1B));
    press(&coord, INPUT);
    let after = coord.debug_page_texts();
    assert_eq!(
        after[0], before[1],
        "`ab` 上置顶的「{}」应成为组合首选",
        shown[1]
    );
    // 首条换了人，显示整串的也跟着换（首条判定在显示出口，不认引擎给的位置）。
    assert_eq!(coord.debug_page_display_texts()[0], after[0]);
}

/// 以词定字按**所见**取字：高亮「② 蒸」时按 `,` 上屏「式蒸」的前段 + 「蒸」，而不是整串
/// 第一个字「式」。
#[test]
fn select_char_on_alt_uses_shown_back_segment() {
    if !has_data() {
        eprintln!("跳过：缺 build_dev 词库");
        return;
    }
    const VK_COMMA: u32 = 0xBC;
    const VK_DOWN: u32 = 0x28;
    let (coord, _store, _dir) = coord_with("selchar", "");
    press(&coord, INPUT);
    let texts = coord.debug_page_texts();
    let shown = coord.debug_page_display_texts();
    let front = texts[1]
        .strip_suffix(shown[1].as_str())
        .expect("前提：② 只显示后段");
    coord.handle_key_event(&key(VK_DOWN));
    let first_shown: String = shown[1].chars().take(1).collect();
    match coord.handle_key_event(&key(VK_COMMA)) {
        KeyAction::InsertText { text, .. } => assert_eq!(text, format!("{front}{first_shown}")),
        other => panic!("以词定字应上屏，实际 {other:?}"),
    }
}

/// 前段多取时，前段**非首选**的行被顶到首位（这里用整串码上的置顶；词频重排同形）：
/// 首条展示的是另一个前段，其余行若仍只显示后段，「① 戒节 ② 节」的 ② 读起来是「戒节」、
/// 上屏却是「式节」⇒ 此时全部显示整串。
#[test]
fn other_front_on_top_shows_every_row_whole() {
    if !has_data() {
        eprintln!("跳过：缺 build_dev 词库");
        return;
    }
    let (coord, store, _dir) = coord_with("front2", "split_front_candidates = 2\n");
    press(&coord, INPUT);
    let before = coord.debug_page_texts();
    let shown = coord.debug_page_display_texts();
    // 前段非首选的行：显示与整串相同的那几条里，排在首条之后的第一条。
    let other = (1..before.len())
        .find(|&i| shown[i] == before[i])
        .map(|i| before[i].clone())
        .expect("前提：前段多取时应有前段非首选的行（整串显示）");
    assert_ne!(shown[1], before[1], "前提：前段首选行的次选只显示后段");
    store.pin_shadow("wubi86", INPUT, &other, None, 0).unwrap();

    coord.handle_key_event(&key(0x1B));
    press(&coord, INPUT);
    let after = coord.debug_page_texts();
    assert_eq!(after[0], other, "前提：置顶生效");
    assert_eq!(
        coord.debug_page_display_texts(),
        after,
        "首条是另一个前段时，每一行都要显示整串"
    );
}

fn seed(code: &str, text: &str) -> wind_phrase::PhraseSeed {
    wind_phrase::PhraseSeed {
        code: code.into(),
        text: text.into(),
        weight: 2000,
        position: 0,
        is_system: true,
        category: String::new(),
    }
}

/// 自造 4 码方案（开逆切分）：`oj`→井、`s`→三/所有，`ojs` 在码表里空码且无后继 ⇒ 未满码
/// 也照切（2+1）。不依赖 `build_dev/data`。
fn coord_xy(tag: &str) -> (Arc<Coordinator>, TempRoot) {
    coord_xy_with(tag, "", |_| {})
}

/// `coord_xy` 的可配版：`extra_dict` 追加词条行，`tweak` 改全局配置。
fn coord_xy_with(
    tag: &str,
    extra_dict: &str,
    tweak: impl FnOnce(&mut Config),
) -> (Arc<Coordinator>, TempRoot) {
    let root = std::env::temp_dir().join(format!("wind_split_xy_{tag}_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let schemas = root.join("data/schemas");
    std::fs::create_dir_all(schemas.join("xy")).unwrap();
    std::fs::write(
        schemas.join("xy.schema.toml"),
        "[schema]\nid = \"xy\"\nname = \"xy\"\n[engine]\ntype = \"codetable\"\n\
         [engine.codetable]\nmax_code_length = 4\nsplit_input = true\n\
         [[dictionaries]]\nid = \"main\"\npath = \"xy/xy.dict.yaml\"\ndefault = true\n",
    )
    .unwrap();
    std::fs::write(
        schemas.join("xy/xy.dict.yaml"),
        format!("---\nname: xy\nversion: \"1\"\n...\n井\toj\n三\ts\n所有\ts\n{extra_dict}"),
    )
    .unwrap();
    let store = Arc::new(Store::open(root.join("user_data.db")).unwrap());
    let mut c = Config::default();
    c.schema.available = vec!["xy".into()];
    c.schema.active = "xy".into();
    c.input.default.chinese_mode = true;
    tweak(&mut c);
    let coord =
        Coordinator::new_headless_with_store_override(c, Some(&root.join("data")), store, None);
    (coord, TempRoot(root))
}

/// 论坛 t307：短语码在码表里空码时，引擎照样切分——直通命令 `ojs` 旁边多出「井三/井所有」，
/// 组合区显示切分串 `oj's`。精确码短语就是这串码的精确解，切分候选须让位。
#[test]
fn exact_code_phrase_suppresses_split() {
    // ★ 前提对照：没有短语时这串码确实切分——否则下面是在一个不切的世界里断言「没切」。
    let (bare, _d0) = coord_xy("t307_bare");
    press(&bare, "ojs");
    assert_eq!(bare.debug_page_texts(), vec!["井三", "井所有"]);
    assert_eq!(bare.debug_preedit(), "oj's");

    for (tag, text, shown) in [
        ("lit", "短语甲", "短语甲"),
        (
            "cmd",
            r#"$CC("[计算器]", proc.run("calc.exe"))"#,
            "[计算器]",
        ),
    ] {
        let (coord, _d) = coord_xy(&format!("t307_{tag}"));
        coord.debug_install_phrases(vec![seed("ojs", text)]);
        press(&coord, "ojs");
        assert_eq!(coord.debug_page_texts(), vec![shown], "{tag}：只剩短语");
        assert_eq!(coord.debug_preedit(), "ojs", "{tag}：组合区不得留切分串");
    }

    // 码在短语表里、但求值为空的条目不出候选（这里是非 Windows 恒空的 `{clip()}`）⇒ 它不是
    // 这串码的解，切分照常——按「码在不在表里」判的话候选窗会全空。
    let (empty, _d2) = coord_xy("t307_empty");
    empty.debug_install_phrases(vec![seed("ojs", "{clip()}")]);
    press(&empty, "ojs");
    assert_eq!(empty.debug_page_texts(), vec!["井三", "井所有"]);
}

/// 同上，满码长（主场）那一格：五笔 `aaab` 空码会切成「式节/式蒸…」。
#[test]
fn exact_code_phrase_suppresses_split_at_full_length() {
    if !has_data() {
        eprintln!("跳过：缺 build_dev 词库");
        return;
    }
    let (coord, _store, _dir) = coord_with("t307_full", "");
    coord.debug_install_phrases(vec![seed(INPUT, "短语甲")]);
    press(&coord, INPUT);
    assert_eq!(coord.debug_page_texts(), vec!["短语甲"]);
}

/// 论坛 t302：精确匹配 + 空码补全 + 逆切分，满码长 `ojss` 空码、但码表收了超码长的 `ojssa`。
/// 期望：不切分，空码补全给出后续编码那一条。修复前两处都错——切成「井三三…」，且补全的
/// `< max_code_length` 闸门在满码长处不放行。
#[test]
fn full_length_empty_code_with_longer_entry_shows_completion_not_split() {
    let exact = |c: &mut Config| {
        c.schema.codetable.single_code_input = true;
        c.schema.codetable.single_code_complete = true;
    };
    let (coord, _d) = coord_xy_with("t302", "后续\tojssa\n", exact);
    press(&coord, "ojss");
    assert_eq!(coord.debug_page_texts(), vec!["后续"]);

    // ★ 反向对照：没有超码长词条时同一串照切——证明拦下切分的是后继。
    let (bare, _d0) = coord_xy_with("t302_bare", "", exact);
    press(&bare, "ojss");
    assert!(
        bare.debug_page_texts().iter().all(|t| t.starts_with('井')),
        "无后继时应切分，实际 {:?}",
        bare.debug_page_texts()
    );
}

/// 论坛 t302：超码长（第 5 码起）空码、仍有更长后续时照样提示后续编码，直到打全。
/// 小鹤音形现场 `okou` → `okouu` → `okouuxb`（𢠙），曾只在满码长那一格提示、第 5 码断开。
/// 竞争者 `ojssbc` 与目标在第 5 码分岔：证明补的是「本串的后续」而非同前缀随便一条。
/// 顶码同开：有更长后继时顶码不得抢先上屏。
#[test]
fn completion_continues_past_full_length() {
    let exact = |c: &mut Config| {
        c.schema.codetable.single_code_input = true;
        c.schema.codetable.single_code_complete = true;
        c.schema.codetable.top_code_commit = true;
    };
    let (coord, _d) = coord_xy_with("t302_over", "后续\tojssab\n另条\tojssbc\n", exact);
    press(&coord, "ojss");
    assert_eq!(coord.debug_page_texts(), vec!["后续"], "满码长");
    press(&coord, "a");
    assert_eq!(coord.debug_input_buffer(), "ojssa", "第 5 码不得顶码");
    assert_eq!(
        coord.debug_page_texts(),
        vec!["后续"],
        "第 5 码仍应提示后续"
    );
    press(&coord, "b");
    assert_eq!(coord.debug_page_texts(), vec!["后续"], "全码");

    let (other, _d1) = coord_xy_with("t302_over_b", "后续\tojssab\n另条\tojssbc\n", exact);
    press(&other, "ojssb");
    assert_eq!(
        other.debug_page_texts(),
        vec!["另条"],
        "第 5 码分岔后补另一条"
    );

    // 短语侧补全源同口径：码表无货、短语收着更长的码。
    let (ph, _d2) = coord_xy_with("t302_over_ph", "后续\tojssab\n", exact);
    ph.debug_install_phrases(vec![seed("ojsxyz", "短语后续")]);
    press(&ph, "ojsxy");
    assert_eq!(ph.debug_page_texts(), vec!["短语后续"], "超码长短语后续");
}

/// 论坛 t302 连带：开 `clear_on_empty_max` 时，超码长的短语后续补全须拦住满码空码清空。
/// 引擎只看码表（`ojsxy` 无后继 ⇒ 给出清空资格），协调器按最终列表裁决——补出的短语后续
/// 在列表里就不清。反例：`ojssz` 码表与短语都无后继，照常清空。
#[test]
fn phrase_completion_past_full_length_blocks_clear_on_empty_max() {
    let exact_clear = |c: &mut Config| {
        c.schema.codetable.single_code_input = true;
        c.schema.codetable.single_code_complete = true;
        c.schema.codetable.clear_on_empty_max = true;
    };
    let (ph, _d) = coord_xy_with("t302_clear_ph", "后续\tojssab\n", exact_clear);
    ph.debug_install_phrases(vec![seed("ojsxyz", "短语后续")]);
    press(&ph, "ojsxy");
    assert_eq!(ph.debug_input_buffer(), "ojsxy", "有短语后续时不得清空");
    assert_eq!(ph.debug_page_texts(), vec!["短语后续"]);

    let (bare, _d1) = coord_xy_with("t302_clear_bare", "后续\tojssab\n", exact_clear);
    bare.debug_install_phrases(vec![seed("ojsxyz", "短语后续")]);
    press(&bare, "ojssz");
    assert_eq!(bare.debug_input_buffer(), "", "无任何后继照常清空");
    assert!(bare.debug_page_texts().is_empty());
}
