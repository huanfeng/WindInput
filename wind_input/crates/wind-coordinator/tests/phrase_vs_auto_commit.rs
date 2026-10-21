//! 短语侧对**全码唯一自动上屏**的否决（`phrase_vetoes_auto_commit`）。
//!
//! 引擎的 `decide_auto_commit` 在**码表候选子集**上判唯一（按 `c.code == input` 筛），而短语
//! 候选的 `code` 恒为空串、且在引擎 `convert` 之后才由协调器追加 ⇒ 短语对那道判据完全不可见。
//! 修复前的症状是「关掉开关候选面有两条、开了反而只剩一条」——显示与处置对不上。
//!
//! 真机现场（v0.118.0，用户反馈 + 日志 + 操作视频三方确认）：用户短语 `aqgy → 东乌珠穆沁旗`
//! （w=1000）与五笔 `aqgy → 葡`（w=1379）同码，敲完第 4 码当场上屏「葡」，候选窗从未显示
//! （日志里按 Y 后只有 HideCandidates、无 UpdateCandidates），用户根本没有按空格的机会。
//!
//! 数据前提：`wubi86_jidian_extra_district.dict.yaml`（内含 `aqgy 东乌珠穆沁旗` w=152525）出厂
//! `default_enabled = false`，故码表侧 `aqgy` 只有「葡」一条——这既是 `decide_auto_commit` 判
//! 「唯一」的前提，也是用户要手动加这条短语的原因。

use std::path::PathBuf;
use wind_bridge::handler::{KeyAction, KeyEventData, MessageHandler};
use wind_config::Config;
use wind_coordinator::Coordinator;
use wind_ipc::protocol::EVENT_KEY_DOWN;

fn data_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../build_dev/data")
}

fn has_schemas() -> bool {
    let d = data_dir();
    d.join("schemas/wubi86.schema.toml").exists() || d.join("schemas/wubi86.schema.yaml").exists()
}

fn key_event(key_code: u32, event_type: u8) -> KeyEventData {
    KeyEventData {
        key_code,
        scan_code: 0,
        modifiers: 0,
        event_type,
        toggles: 0,
        event_seq: 0,
        prev_char: 0,
    }
}

fn press_letter(coord: &Coordinator, c: char) -> KeyAction {
    let vk = (c.to_ascii_uppercase() as u32) & 0xFF;
    coord.handle_key_event(&key_event(vk, EVENT_KEY_DOWN))
}

/// 建一个 wubi86 协调器，注入一条短语；`at_full` 决定是否开「全码唯一自动上屏」。
fn coord_with_phrase(
    tag: &str,
    code: &str,
    text: &str,
    weight: i32,
    at_full: bool,
) -> (std::sync::Arc<Coordinator>, PathBuf) {
    let store_path = std::env::temp_dir().join(format!("wind_phrase_ac_{tag}.redb"));
    let _ = std::fs::remove_file(&store_path);
    let store = std::sync::Arc::new(wind_store::Store::open(&store_path).unwrap());
    store.add_phrase(code, text, 0, weight).unwrap();

    let mut cfg = Config::default();
    cfg.schema.available = vec!["wubi86".into(), "pinyin".into()];
    cfg.schema.active = "wubi86".into();
    cfg.input.default.chinese_mode = true;
    cfg.schema.codetable.auto_commit_at_full = at_full;
    (
        Coordinator::new_headless_with_store(cfg, Some(&data_dir()), store),
        store_path,
    )
}

/// 敲 `aqg` 三码后按第 4 码 `y`（VK 0x59），返回该键的动作与其后的候选面。
fn drive_aqgy(coord: &Coordinator) -> (KeyAction, Vec<String>) {
    for ch in ['a', 'q', 'g'] {
        press_letter(coord, ch);
    }
    let action = coord.handle_key_event(&key_event(0x59, EVENT_KEY_DOWN));
    (action, coord.debug_all_candidate_texts())
}

/// 同码短语在场时，全码唯一自动上屏必须让位——否则用户配的短语连露面的机会都没有。
#[test]
fn auto_commit_at_full_yields_to_same_code_phrase() {
    if !has_schemas() {
        eprintln!("跳过：缺少 build_dev/data/schemas —— 本测试未实际执行");
        return;
    }
    // 权重与用户截图一致（1000，词库管理页默认值）：**低于**码表「葡」的 1379。
    // 这个高低关系是修复前触发 bug 的必要条件之一，故必须照抄，不可随手写个大数。
    let (coord, store_path) = coord_with_phrase("same", "aqgy", "东乌珠穆沁旗", 1000, true);
    let (action, cands) = drive_aqgy(&coord);
    let _ = std::fs::remove_file(&store_path);

    if let KeyAction::InsertText { text, .. } = &action {
        panic!("整串已是精确码短语时不得自动上屏，实际上屏了「{text}」；候选面={cands:?}");
    }
    assert!(
        cands.iter().any(|t| t == "东乌珠穆沁旗") && cands.iter().any(|t| t == "葡"),
        "让位后两条候选都应在候选面上，实际: {cands:?}"
    );
}

/// **码长超过方案满码长的短语**不得被自动上屏劫走（`has_longer_code` 那一半判据的守门测试）。
///
/// 5 码短语 `aqgyz` 落在 4 码五笔方案里：敲到 `aqgy` 时码表侧恰是「精确唯一 + 无更长后继」
/// ——正是自动上屏最爱命中的形态。一旦在这里上屏「葡」，`aqgyz` 这条短语**永远打不出来**。
/// 与顶码那侧的 `top_code_yields_to_overlong_phrase` 是同一类事故的两条路径。
#[test]
fn auto_commit_at_full_yields_to_overlong_phrase() {
    if !has_schemas() {
        eprintln!("跳过：缺少 build_dev/data/schemas —— 本测试未实际执行");
        return;
    }
    let (coord, store_path) = coord_with_phrase("overlong", "aqgyz", "超长码短语", 1000, true);
    let (action, cands) = drive_aqgy(&coord);
    let _ = std::fs::remove_file(&store_path);

    if let KeyAction::InsertText { text, .. } = &action {
        panic!(
            "还能续打成更长短语时不得自动上屏，实际上屏了「{text}」\
             （aqgyz 将永远打不出来）；候选面={cands:?}"
        );
    }
}

/// 对照：短语权重高于同码码表候选时，首选本就是短语，既有守护（首选须是码表来源）已能拦住。
/// 与上面那条一起划出 bug 的边界——修复前**仅在权重输时**被绕过，故权重不是可有可无的前提。
#[test]
fn higher_weight_phrase_takes_first_and_blocks_auto_commit() {
    if !has_schemas() {
        eprintln!("跳过：缺少 build_dev/data/schemas —— 本测试未实际执行");
        return;
    }
    let (coord, store_path) = coord_with_phrase("hiweight", "aqgy", "东乌珠穆沁旗", 9999, true);
    let (action, cands) = drive_aqgy(&coord);
    let _ = std::fs::remove_file(&store_path);

    assert!(
        matches!(action, KeyAction::UpdateComposition { .. }),
        "短语权重占优时不应自动上屏，实际: {action:?}"
    );
    assert_eq!(
        cands.first().map(String::as_str),
        Some("东乌珠穆沁旗"),
        "权重占优的短语应排首位，实际: {cands:?}"
    );
}

/// 对照：开关关闭（出厂默认）时行为不变。与第一条合起来即用户的核心诉求——
/// **开与不开，候选面必须是同一份**。
#[test]
fn candidates_identical_whether_auto_commit_enabled() {
    if !has_schemas() {
        eprintln!("跳过：缺少 build_dev/data/schemas —— 本测试未实际执行");
        return;
    }
    let (coord_off, path_off) = coord_with_phrase("off", "aqgy", "东乌珠穆沁旗", 1000, false);
    let (action_off, cands_off) = drive_aqgy(&coord_off);
    let _ = std::fs::remove_file(&path_off);

    let (coord_on, path_on) = coord_with_phrase("on", "aqgy", "东乌珠穆沁旗", 1000, true);
    let (_action_on, cands_on) = drive_aqgy(&coord_on);
    let _ = std::fs::remove_file(&path_on);

    assert!(
        matches!(action_off, KeyAction::UpdateComposition { .. }),
        "开关关闭时第 4 码不应自动上屏，实际: {action_off:?}"
    );
    assert_eq!(
        cands_off, cands_on,
        "开与不开自动上屏，候选面必须一致（这正是本次修复的诉求）"
    );
}

/// 论坛 t308：双拼关简拼后，`ofa` 没有任何拼音候选，短语（这里是直通命令）成了唯一一条，
/// 打完第三码就被「全码唯一自动上屏」当场执行——拼音系没有满码，这条开关本不该管它。
///
/// 开关读的是 `schema.codetable`，拼音方案继承全局那一份，故用户为码表开了它，双拼就中招。
/// 用普通短语断言（命令短语走同一道闸，结局只是 AutoCommand 而非 InsertText）。
#[test]
fn shuangpin_phrase_is_not_auto_committed() {
    let d = data_dir();
    if !d.join("schemas/shuangpin.schema.toml").exists() {
        eprintln!("跳过：缺少 build_dev/data 双拼方案");
        return;
    }
    let store_path =
        std::env::temp_dir().join(format!("wind_phrase_ac_t308_{}.redb", std::process::id()));
    let _ = std::fs::remove_file(&store_path);
    let store = std::sync::Arc::new(wind_store::Store::open(&store_path).unwrap());
    store.add_phrase("ofa", "音形短语", 0, 1200).unwrap();

    let mut cfg = Config::default();
    cfg.schema.available = vec!["shuangpin".into()];
    cfg.schema.active = "shuangpin".into();
    cfg.input.default.chinese_mode = true;
    cfg.schema.pinyin.abbrev.enabled = false;
    cfg.schema.codetable.auto_commit_at_full = true;
    let coord = Coordinator::new_headless_with_store(cfg, Some(&d), store);

    press_letter(&coord, 'o');
    press_letter(&coord, 'f');
    let action = press_letter(&coord, 'a');
    let cands = coord.debug_all_candidate_texts();
    let _ = std::fs::remove_file(&store_path);
    assert!(
        !matches!(action, KeyAction::InsertText { .. }),
        "双拼下短语不得被自动上屏，实际 {action:?}"
    );
    assert_eq!(
        cands,
        vec!["音形短语"],
        "前提：短语须是唯一候选（否则唯一闸就拦了，测不到本缺口）"
    );
}

/// 建一个五笔拼音混输协调器（开满码自动上屏），注入一条短语。
fn mixed_coord_with_phrase(
    tag: &str,
    code: &str,
    text: &str,
) -> (std::sync::Arc<Coordinator>, PathBuf) {
    let store_path = std::env::temp_dir().join(format!(
        "wind_phrase_ac_mixed_{tag}_{}.redb",
        std::process::id()
    ));
    let _ = std::fs::remove_file(&store_path);
    let store = std::sync::Arc::new(wind_store::Store::open(&store_path).unwrap());
    store.add_phrase(code, text, 0, 1200).unwrap();

    let mut cfg = Config::default();
    cfg.schema.available = vec!["wubi86_pinyin".into()];
    cfg.schema.active = "wubi86_pinyin".into();
    cfg.input.default.chinese_mode = true;
    cfg.schema.codetable.auto_commit_at_full = true;
    (
        Coordinator::new_headless_with_store(cfg, Some(&data_dir()), store),
        store_path,
    )
}

fn has_mixed_schema() -> bool {
    data_dir()
        .join("schemas/wubi86_pinyin.schema.toml")
        .exists()
}

/// 混输是码表系，有满码：满码唯一的短语照常自动上屏。
///
/// 拼音 / 双拼的拦截（`shuangpin_phrase_is_not_auto_committed`）曾按 `max_code_length == 0`
/// 认引擎，而混输引擎这个值刻意为 0，于是混输也被一并拦下。
#[test]
fn mixed_full_code_phrase_is_auto_committed() {
    if !has_mixed_schema() {
        eprintln!("跳过：缺少 build_dev/data 混输方案");
        return;
    }
    let (coord, store_path) = mixed_coord_with_phrase("full", "zqzq", "混输满码短语");
    let mut action = KeyAction::Consumed;
    for ch in "zqzq".chars() {
        action = press_letter(&coord, ch);
    }
    let _ = std::fs::remove_file(&store_path);
    assert!(
        matches!(&action, KeyAction::InsertText { text, .. } if text == "混输满码短语"),
        "混输下满码唯一短语应自动上屏，实际 {action:?}"
    );
}

/// 混输下未到满码的短语不自动上屏（最短码长闸须按主码表码长算，而不是 0）。
#[test]
fn mixed_short_phrase_is_not_auto_committed() {
    if !has_mixed_schema() {
        eprintln!("跳过：缺少 build_dev/data 混输方案");
        return;
    }
    let (coord, store_path) = mixed_coord_with_phrase("short", "zqz", "混输三码短语");
    let mut action = KeyAction::Consumed;
    for ch in "zqz".chars() {
        action = press_letter(&coord, ch);
    }
    let cands = coord.debug_all_candidate_texts();
    let _ = std::fs::remove_file(&store_path);
    assert!(
        !matches!(action, KeyAction::InsertText { .. }),
        "混输下 3 码短语不得自动上屏，实际 {action:?}"
    );
    assert_eq!(
        cands,
        vec!["混输三码短语"],
        "前提：短语须是唯一候选（否则唯一闸就拦了，测不到码长闸）"
    );
}

/// 建一个 wubi86 协调器（开满码自动上屏），注入一条短语；`sentence` 经方案 override 开整句。
///
/// 返回顺序 `(store 路径, override 目录, 协调器)`：绑定按声明逆序析构 ⇒ 协调器先放掉，
/// 调用方再删文件。
fn sentence_coord_with_phrase(
    tag: &str,
    code: &str,
    text: &str,
    sentence: bool,
) -> (PathBuf, PathBuf, std::sync::Arc<Coordinator>) {
    let root = std::env::temp_dir().join(format!(
        "wind_phrase_ac_sentence_{tag}_{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&root);
    let ov = root.join("overrides");
    std::fs::create_dir_all(&ov).unwrap();
    std::fs::write(
        ov.join("wubi86.toml"),
        format!("[engine.codetable]\nsentence_input = {sentence}\n"),
    )
    .unwrap();
    let store_path = root.join("user_data.redb");
    let store = std::sync::Arc::new(wind_store::Store::open(&store_path).unwrap());
    store.add_phrase(code, text, 0, 1200).unwrap();

    let mut cfg = Config::default();
    cfg.schema.available = vec!["wubi86".into()];
    cfg.schema.active = "wubi86".into();
    cfg.input.default.chinese_mode = true;
    cfg.schema.codetable.auto_commit_at_full = true;
    let coord =
        Coordinator::new_headless_with_store_override(cfg, Some(&data_dir()), store, Some(ov));
    (store_path, root, coord)
}

/// 短语满码上屏读的是配置、绕过引擎，整句开启时须在协调器侧另行让位。
///
/// `qwrr` 在五笔 86 里是空码，挂上一条短语后它就是唯一候选、且满码——正是短语满码上屏
/// 的命中形态。两向对照：关整句时上屏（证明现场确实走到了短语上屏），开整句时不上屏。
#[test]
fn phrase_auto_commit_yields_to_sentence_input() {
    if !has_schemas() {
        eprintln!("跳过：缺少 build_dev/data/schemas —— 本测试未实际执行");
        return;
    }
    for sentence in [false, true] {
        let tag = if sentence { "on" } else { "off" };
        let (_, root, coord) = sentence_coord_with_phrase(tag, "qwrr", "空码短语", sentence);
        let mut action = KeyAction::Consumed;
        for ch in "qwrr".chars() {
            action = press_letter(&coord, ch);
        }
        drop(coord);
        let _ = std::fs::remove_dir_all(&root);
        if sentence {
            assert!(
                matches!(action, KeyAction::UpdateComposition { .. }),
                "开整句时短语不得满码上屏，实际 {action:?}"
            );
        } else {
            assert!(
                matches!(&action, KeyAction::InsertText { text, .. } if text == "空码短语"),
                "前提：关整句时唯一短语应满码上屏，实际 {action:?}"
            );
        }
    }
}
