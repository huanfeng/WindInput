//! 辅助码流程的无头集成测试：方案 data_dir（含 `[engine.aux_code]` + `[key_actions]`）+
//! 临时 store。headless 无引擎，故候选由测试直接装填，覆盖的是辅助码自身的进出/筛选/
//! 还原/上屏语义（`aux_code_settings` 经真实方案文件解析，证明配置接线的正确性）。
//!
//! 多数用例的 fixture 方案写了 `enabled = true`，而出厂是关的；开关三态由
//! `aux_code_disabled_*` 那组用例单独覆盖。
use super::*;
use crate::coordinator::Coordinator;
use std::sync::Arc;
use wind_bridge::handler::KeyEventData;
use wind_candidate::{Candidate, CandidateSource};
use wind_config::Config;
use wind_ipc::protocol::{MOD_ALT, MOD_CTRL};
use wind_store::Store;

/// 造一个含 pinyin 方案的 data_dir：`[engine.aux_code]` 指到测试小码表，backtick 绑辅助码。
/// 方案段显式 `enabled = true`（出厂全局是 false，见模块头注释）。
fn data_dir_with_aux(tag: &str) -> std::path::PathBuf {
    data_dir_with_aux_enabled(tag, Some(true))
}

/// 同上，但方案段的 `enabled` 可控：`None` = 不写这一行（回落全局基线）。
fn data_dir_with_aux_enabled(tag: &str, enabled: Option<bool>) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("wind_aux_code_data_{tag}"));
    let _ = std::fs::remove_dir_all(&dir);
    let schemas = dir.join("schemas");
    let aux_code_dir = schemas.join("aux_code");
    std::fs::create_dir_all(&aux_code_dir).unwrap();
    let enabled_line = match enabled {
        Some(v) => format!("enabled = {v}\n"),
        None => String::new(),
    };
    std::fs::write(
        schemas.join("pinyin.schema.toml"),
        format!(
            "[schema]\nid = \"pinyin\"\nname = \"pinyin\"\n\
                 [engine]\ntype = \"pinyin\"\n\
                 [engine.aux_code]\nfiles = [\"aux_code/flypy_test.txt\"]\n{enabled_line}\
                 [key_actions]\nbacktick = \"aux_code\"\n"
        ),
    )
    .unwrap();
    std::fs::write(aux_code_dir.join("flypy_test.txt"), "李=mz\n樱=my\n河=sk\n").unwrap();
    dir
}

fn coord_with(tag: &str) -> Arc<Coordinator> {
    coord_with_data(tag, data_dir_with_aux(tag))
}

fn coord_with_data(tag: &str, data_dir: std::path::PathBuf) -> Arc<Coordinator> {
    coord_with_data_cfg(tag, data_dir, |_| {})
}

/// 同上，但可改配置。出厂值恰好让某些差异不可见（如 `per_page_extended = 0` 时
/// 两档相同），不显式配上就测不出档位选错。
fn coord_with_data_cfg(
    tag: &str,
    data_dir: std::path::PathBuf,
    tweak: impl FnOnce(&mut Config),
) -> Arc<Coordinator> {
    let path = std::env::temp_dir().join(format!("wind_aux_code_{tag}.redb"));
    let _ = std::fs::remove_file(&path);
    let store = Arc::new(Store::open(&path).unwrap());
    let mut cfg = Config::default();
    cfg.schema.active = "pinyin".to_string();
    tweak(&mut cfg);
    Coordinator::new_headless_with_store(cfg, Some(&data_dir), store)
}

fn cand(text: &str) -> Candidate {
    Candidate {
        text: text.into(),
        source: CandidateSource::Pinyin,
        ..Default::default()
    }
}

/// 字母 VK（VK_A..=VK_Z 是区间端点常量，逐字命名的仅 A/Z）。
fn vk_letter(c: char) -> u32 {
    keymap::VK_A + (c as u32 - 'A' as u32)
}

fn key(vk: u32, modifiers: u32) -> KeyEventData {
    KeyEventData {
        key_code: vk,
        scan_code: 0,
        modifiers,
        event_type: 0,
        toggles: 0,
        event_seq: 0,
        prev_char: 0,
        seq_char: 0,
    }
}

/// 装填「拼音组合中」的初始状态：缓冲 + 候选（李/樱/河），高亮首选。
fn seed_composition(c: &Arc<Coordinator>) -> std::sync::MutexGuard<'_, crate::coordinator::State> {
    let mut st = c.state.lock().unwrap();
    st.chinese_mode = true;
    st.input_buffer = "li".to_string();
    st.candidates = vec![cand("李"), cand("樱"), cand("河")];
    st.selected_index = 0;
    st.current_page = 0;
    st.preedit = "li".to_string();
    st
}

#[test]
fn enter_requires_candidates() {
    let c = coord_with("guard");
    let mut st = c.state.lock().unwrap();
    st.chinese_mode = true;
    // 空候选（空缓冲场景）：门卫拦下，触发键不吞（返回 None → 落普通标点）。
    assert!(c.enter_aux_code(&mut st, keymap::VK_BACKTICK).is_none());
    assert_eq!(st.active, None);
}

#[test]
fn enter_sets_mode_and_preserves_pinyin_preedit() {
    let c = coord_with("enter");
    let mut st = seed_composition(&c);
    let act = c
        .enter_aux_code(&mut st, keymap::VK_BACKTICK)
        .expect("有候选应进入");
    assert_eq!(st.active, Some(ModeKind::AuxCode));
    let overlay = st.aux_code.as_ref().expect("辅助码 overlay 已建立");
    assert!(overlay.session.is_empty());
    assert_eq!(overlay.preedit_base, "li");
    // 组合区 = 拼音 + 4 空格 + 辅助码（空）。
    assert_eq!(st.preedit, "li    ");
    assert!(matches!(act, KeyAction::UpdateComposition { .. }));
}

#[test]
fn aux_letters_filter_and_drop_tail() {
    let c = coord_with("filter");
    let mut st = seed_composition(&c);
    let _ = c.enter_aux_code(&mut st, keymap::VK_BACKTICK);
    // 打 m：李(mz)/樱(my) 命中，河(sk) 被滤 → 直接丢弃，不在候选列表里。
    let act = c.handle_aux_code_key(&mut st, &key(vk_letter('M'), 0));
    assert!(matches!(act, KeyAction::UpdateComposition { .. }));
    let texts: Vec<&str> = st.candidates.iter().map(|c| c.text.as_str()).collect();
    assert_eq!(
        texts,
        vec!["李", "樱"],
        "筛选后 = 命中子序列，被滤候选不再出现在候选窗"
    );
    assert_eq!(st.preedit, "li    m");
}

#[test]
fn deeper_aux_code_narrows_again() {
    let c = coord_with("narrow");
    let mut st = seed_composition(&c);
    let _ = c.enter_aux_code(&mut st, keymap::VK_BACKTICK);
    let _ = c.handle_aux_code_key(&mut st, &key(vk_letter('M'), 0));
    // 再打 y：只剩 樱(my)。
    let _ = c.handle_aux_code_key(&mut st, &key(vk_letter('Y'), 0));
    let kept: Vec<&str> = st.candidates.iter().map(|c| c.text.as_str()).collect();
    assert_eq!(kept, vec!["樱"]);
    assert_eq!(st.preedit, "li    my");
}

#[test]
fn esc_exits_and_restores_pinyin_composition() {
    let c = coord_with("esc");
    let mut st = seed_composition(&c);
    let _ = c.enter_aux_code(&mut st, keymap::VK_BACKTICK);
    let _ = c.handle_aux_code_key(&mut st, &key(vk_letter('M'), 0));
    let act = c.handle_aux_code_key(&mut st, &key(keymap::VK_ESCAPE, 0));
    assert_eq!(st.active, None, "Esc 退出辅助码");
    assert_eq!(st.preedit, "li", "退出还原拼音组合区");
    assert!(
        st.aux_code.is_none(),
        "退出销毁整个 overlay（会话/基线/前缀）"
    );
    assert_eq!(st.candidates.len(), 3);
    assert!(matches!(act, KeyAction::UpdateComposition { .. }));
}

/// 自定义取消键（`session_actions` 里的 `cancel`）在辅助码态必须连主组合一起放弃，不能只退筛选。
///
/// `exit_aux_code` 会还原拼音候选与 preedit，而 `cancel_session` 末尾无条件发
/// `ClearComposition`；两者拼在一起时宿主清了组合、协调器 `input_buffer` 却还在。判据取
/// 「协调器状态与 ClearComposition 相符」，返回值修不修都是 ClearComposition。
/// 对照 `esc_exits_and_restores_pinyin_composition`：Esc 还原拼音，取消键整体放弃。
#[test]
fn cancel_session_in_aux_mode_clears_whole_composition() {
    let c = coord_with("cancel_whole");
    let mut st = seed_composition(&c);
    let _ = c.enter_aux_code(&mut st, keymap::VK_BACKTICK);
    let _ = c.handle_aux_code_key(&mut st, &key(vk_letter('M'), 0));
    assert_eq!(st.active, Some(ModeKind::AuxCode));

    let act = c.cancel_session(&mut st);
    assert!(matches!(act, KeyAction::ClearComposition));
    assert_eq!(st.active, None, "取消键应退出辅助码");
    assert!(st.aux_code.is_none(), "overlay 三件套应整体销毁");
    assert!(
        st.input_buffer.is_empty(),
        "编码缓冲必须清空——残留会让下一次按键在屏幕上补出旧内容"
    );
    assert!(
        st.preedit.is_empty(),
        "组合区必须清空，与 ClearComposition 相符"
    );
    assert!(
        st.candidates.is_empty(),
        "候选必须清空，UI 已被 notify_ui_hide 隐藏"
    );
}

#[test]
fn backspace_to_empty_stays_for_reinput() {
    let c = coord_with("back");
    let mut st = seed_composition(&c);
    let _ = c.enter_aux_code(&mut st, keymap::VK_BACKTICK);
    let _ = c.handle_aux_code_key(&mut st, &key(vk_letter('M'), 0));
    let _ = c.handle_aux_code_key(&mut st, &key(keymap::VK_BACK, 0));
    // 删空后保持在辅助码态（空码、候选还原），方便重新输入。
    assert_eq!(st.active, Some(ModeKind::AuxCode), "删空后不退出辅助码");
    assert!(st.aux_code.as_ref().unwrap().session.is_empty());
    assert_eq!(st.candidates.len(), 3);
    assert_eq!(st.preedit, "li    ");
    // 直接再输辅助码即可重新筛选。
    let _ = c.handle_aux_code_key(&mut st, &key(vk_letter('M'), 0));
    let kept: Vec<&str> = st.candidates.iter().map(|c| c.text.as_str()).collect();
    assert_eq!(kept, vec!["李", "樱"]);
    // 空码再退格 → 退出辅助码、还原拼音组合。
    let _ = c.handle_aux_code_key(&mut st, &key(keymap::VK_BACK, 0));
    assert_eq!(st.active, Some(ModeKind::AuxCode), "删空这一下保持空码态");
    let act = c.handle_aux_code_key(&mut st, &key(keymap::VK_BACK, 0));
    assert_eq!(st.active, None, "空码再退格退出辅助码");
    assert_eq!(st.preedit, "li");
    assert!(
        st.aux_code.is_none(),
        "退格退出销毁整个 overlay（会话/基线/前缀）"
    );
    assert_eq!(st.candidates.len(), 3);
    assert!(matches!(act, KeyAction::UpdateComposition { .. }));
}

#[test]
fn backspace_restores_previous_filter_level() {
    let c = coord_with("prev");
    let mut st = seed_composition(&c);
    let _ = c.enter_aux_code(&mut st, keymap::VK_BACKTICK);
    // my → 只剩 樱。
    let _ = c.handle_aux_code_key(&mut st, &key(vk_letter('M'), 0));
    let _ = c.handle_aux_code_key(&mut st, &key(vk_letter('Y'), 0));
    let kept: Vec<&str> = st.candidates.iter().map(|c| c.text.as_str()).collect();
    assert_eq!(kept, vec!["樱"]);
    // 退格删 y → 回到 m 的筛选层（还原到"之前的状态"）。
    let _ = c.handle_aux_code_key(&mut st, &key(keymap::VK_BACK, 0));
    let kept: Vec<&str> = st.candidates.iter().map(|c| c.text.as_str()).collect();
    assert_eq!(kept, vec!["李", "樱"], "退格还原到上一层筛选结果");
    assert_eq!(st.preedit, "li    m");
    // 再退格删空 → 保持辅助码态，候选全部还原，组合区回空码显示。
    let _ = c.handle_aux_code_key(&mut st, &key(keymap::VK_BACK, 0));
    assert_eq!(st.active, Some(ModeKind::AuxCode), "删空后不退出");
    assert_eq!(st.candidates.len(), 3);
    assert_eq!(st.preedit, "li    ");
}

#[test]
fn wrong_aux_code_backspace_restores_all() {
    let c = coord_with("wrong");
    let mut st = seed_composition(&c);
    let _ = c.enter_aux_code(&mut st, keymap::VK_BACKTICK);
    // 错误辅助码 zz：一个都匹配不上，候选列表清空（被滤候选直接丢弃）。
    let _ = c.handle_aux_code_key(&mut st, &key(vk_letter('Z'), 0));
    let _ = c.handle_aux_code_key(&mut st, &key(vk_letter('Z'), 0));
    assert!(
        st.candidates.is_empty(),
        "错误码下候选窗为空（全部被滤丢弃）"
    );
    // 删一个 → 仍在辅助码态（zz → z，照样全滤）。
    let _ = c.handle_aux_code_key(&mut st, &key(keymap::VK_BACK, 0));
    assert_eq!(st.active, Some(ModeKind::AuxCode));
    assert!(
        st.candidates.is_empty(),
        "z 仍匹配不上任何候选，列表保持为空"
    );
    // 删空 → 保持辅助码态，候选栏还原到没筛选的原列表（顺序一致），便于重输。
    let act = c.handle_aux_code_key(&mut st, &key(keymap::VK_BACK, 0));
    assert_eq!(st.active, Some(ModeKind::AuxCode), "删空后不退出辅助码");
    let texts: Vec<&str> = st.candidates.iter().map(|c| c.text.as_str()).collect();
    assert_eq!(texts, vec!["李", "樱", "河"]);
    assert!(matches!(act, KeyAction::UpdateComposition { .. }));
}

#[test]
fn space_commits_highlighted_and_exits() {
    let c = coord_with("space");
    let mut st = seed_composition(&c);
    let _ = c.enter_aux_code(&mut st, keymap::VK_BACKTICK);
    let _ = c.handle_aux_code_key(&mut st, &key(vk_letter('M'), 0));
    // 首选仍是「李」（选中下标 0 未移动，排序保持子序列）。
    let act = c.handle_aux_code_key(&mut st, &key(keymap::VK_SPACE, 0));
    assert_eq!(st.active, None, "上屏后退出辅助码");
    assert!(
        st.aux_code.is_none(),
        "上屏后销毁整个 overlay（会话/基线/前缀）"
    );
    assert!(matches!(act, KeyAction::InsertText { .. }));
}

#[test]
fn ctrl_alt_guard_does_not_consume_as_code() {
    let c = coord_with("ctrl");
    {
        let mut st = seed_composition(&c);
        let _ = c.enter_aux_code(&mut st, keymap::VK_BACKTICK);
        // Ctrl+M 不被当辅助码字符：有输入态 → 清掉组合并把键交还宿主（宿主快捷键）。
        let act = c.handle_aux_code_key(&mut st, &key(vk_letter('M'), MOD_CTRL));
        assert_eq!(st.active, None);
        assert!(matches!(act, KeyAction::ClearCompositionThenPassThrough));
    }
    // 无输入态 → PassThrough。
    let mut st2 = seed_composition(&c);
    let _ = c.enter_aux_code(&mut st2, keymap::VK_BACKTICK);
    st2.input_buffer.clear();
    st2.committed_text.clear();
    let act = c.handle_aux_code_key(&mut st2, &key(vk_letter('M'), MOD_ALT));
    assert!(matches!(act, KeyAction::PassThrough));
}

/// 方案切换会失效辅助码表缓存：切方案后再次进入辅助码必须按新表重筛。缓存全局一份、
/// 不区分方案；用「重写码表文件 + `invalidate_aux_code_table`」模拟切方案钩子。
#[test]
fn schema_switch_invalidates_aux_code_table() {
    let c = coord_with("switch");
    let aux_file = std::env::temp_dir()
        .join("wind_aux_code_data_switch")
        .join("schemas/aux_code/flypy_test.txt");
    let mut st = seed_composition(&c);
    // 首次进入：载入旧表（李=mz/樱=my/河=sk），打 m 命中李+樱。
    let _ = c.enter_aux_code(&mut st, keymap::VK_BACKTICK);
    let _ = c.handle_aux_code_key(&mut st, &key(vk_letter('M'), 0));
    let kept: Vec<&str> = st.candidates.iter().map(|c| c.text.as_str()).collect();
    assert_eq!(kept, vec!["李", "樱"], "旧表：m 命中李/樱");
    let _ = c.handle_aux_code_key(&mut st, &key(keymap::VK_ESCAPE, 0));
    // 模拟方案切换后码表被替换：写新表（李=zz/樱=zy/河=zk，首行换名）。
    std::fs::write(&aux_file, "# name: 新表\n李=zz\n樱=zy\n河=zk\n").unwrap();
    c.invalidate_aux_code_table(); // 切方案钩子置 None，下次进入重挂
    // 再次进入：缓存若没失效会沿用旧表（打 m 仍命中）；失效后按新表（全滤为空）。
    let _ = c.enter_aux_code(&mut st, keymap::VK_BACKTICK);
    let _ = c.handle_aux_code_key(&mut st, &key(vk_letter('M'), 0));
    assert!(
        st.candidates.is_empty(),
        "切方案后必须重挂新表：m 不再命中任何候选"
    );
}

/// 连续组句：选中只消费缓冲前缀的字词（部分消费）→ 不退出辅助码模式，重建会话
/// 继续筛下一段。用「李」consumed_length=1 < 缓冲 "li"(2) 制造逐步转换态；headless
/// 无引擎，重转剩余编码得空候选，但模式态 / 前缀推进 / 会话重建必须正确。
#[test]
fn partial_commit_stays_in_aux_mode_for_continuous_filter() {
    let c = coord_with("continuous");
    let mut st = seed_composition(&c);
    st.candidates[0].consumed_length = 1; // 李 只消费 li 的前 1 码
    let _ = c.enter_aux_code(&mut st, keymap::VK_BACKTICK);
    let _ = c.handle_aux_code_key(&mut st, &key(vk_letter('M'), 0)); // 筛 m → 李/樱
    assert_eq!(st.active, Some(ModeKind::AuxCode));
    // 空格选 李：部分消费 → 逐步转换 + 重建会话，留在辅助码模式。
    let act = c.handle_aux_code_key(&mut st, &key(keymap::VK_SPACE, 0));
    assert_eq!(
        st.active,
        Some(ModeKind::AuxCode),
        "部分消费后不退出辅助码，继续筛下一段"
    );
    assert!(st.aux_code.is_some(), "overlay 仍在（重建的会话）");
    assert_eq!(st.committed_text, "李", "逐步转换：已转换前缀并入");
    assert_eq!(st.input_buffer, "i", "缓冲裁剪为剩余编码");
    assert!(
        st.aux_code.as_ref().unwrap().session.is_empty(),
        "重建会话辅助码缓冲清空，可继续输入下一段辅助码"
    );
    assert!(
        st.preedit.starts_with("李i"),
        "组合区 = 已转换前缀 + 剩余拼音 + 分隔符前缀"
    );
    assert!(matches!(act, KeyAction::UpdateComposition { .. }));
}

/// 第三条选词路径（`select_page_candidate`，修饰键作二三候选键的 keyup 落点）也要留在模式内，
/// 否则按 `2` 能继续组句、轻敲 Shift 选同一个候选却退出辅助码。
///
/// 判据取「`active` 仍是 AuxCode 且缓冲已裁剪」：留在模式内与退出都返回 `UpdateComposition`。
#[test]
fn select_page_candidate_partial_commit_stays_in_aux_mode() {
    let c = coord_with("continuous_selectkey");
    let mut st = seed_composition(&c);
    st.candidates[0].consumed_length = 1; // 李 只消费 li 的前 1 码
    let _ = c.enter_aux_code(&mut st, keymap::VK_BACKTICK);
    let _ = c.handle_aux_code_key(&mut st, &key(vk_letter('M'), 0)); // 筛 m → 李/樱
    assert_eq!(st.active, Some(ModeKind::AuxCode));

    // offset 0 = 首选（李）。走的是与键盘数字键不同的那条路径。
    let act = c
        .select_page_candidate(&mut st, 0)
        .expect("页内有候选，不应越界");
    assert_eq!(
        st.active,
        Some(ModeKind::AuxCode),
        "部分消费后不得退出辅助码——否则与按 `2` 选同一个候选结果不同"
    );
    assert!(st.aux_code.is_some(), "overlay 三件套应已重建");
    assert_eq!(st.committed_text, "李");
    assert_eq!(st.input_buffer, "i", "缓冲应裁剪为剩余编码");
    assert!(
        st.aux_code.as_ref().unwrap().session.is_empty(),
        "重建会话的辅助码缓冲应清空"
    );
    assert!(matches!(act, KeyAction::UpdateComposition { .. }));
}

/// 同一条路径的反向对照：完整消费仍须退出，别把「不退出」写成无条件的。
#[test]
fn select_page_candidate_full_commit_exits_aux_mode() {
    let c = coord_with("continuous_selectkey_exit");
    let mut st = seed_composition(&c);
    // consumed_length 全 0 = 整串消费。
    let _ = c.enter_aux_code(&mut st, keymap::VK_BACKTICK);
    let _ = c.select_page_candidate(&mut st, 0).expect("页内有候选");
    assert_eq!(st.active, None, "整串消费仍退出辅助码");
    assert!(st.aux_code.is_none(), "overlay 三件套应整体销毁");
}

/// 完整消费（候选消费整串）→ 照常退出辅助码：连续组句只在逐步转换态续命，
/// 最后一段选中即整体上屏并退出（对齐 `space_commits_highlighted_and_exits`）。
#[test]
fn full_commit_still_exits_aux_mode() {
    let c = coord_with("continuous_exit");
    let mut st = seed_composition(&c);
    // 全部候选 consumed_length=0（未标注 = 整串消费）→ 空格选中即完整上屏退出。
    let _ = c.enter_aux_code(&mut st, keymap::VK_BACKTICK);
    let act = c.handle_aux_code_key(&mut st, &key(keymap::VK_SPACE, 0));
    assert_eq!(st.active, None, "整串消费仍退出辅助码");
    assert!(st.aux_code.is_none());
    assert!(matches!(act, KeyAction::InsertText { .. }));
}

/// 回归：辅助码模式下按 ↓ 触发的「翻页放宽」（`expand_candidates`）不得把未过滤的
/// 整池候选塞回来——过滤结果必须保持（`has_more`/`candidate_input` 满足扩展条件时，
/// 无守卫会重建整池、丢失筛选）。
#[test]
fn arrow_navigation_does_not_lose_filter() {
    let c = coord_with("nav");
    let mut st = seed_composition(&c);
    let _ = c.enter_aux_code(&mut st, keymap::VK_BACKTICK);
    // 筛 m → [李, 樱]，并制造「池未穷尽、可翻页放宽」的条件。
    let _ = c.handle_aux_code_key(&mut st, &key(vk_letter('M'), 0));
    st.has_more = true;
    st.candidate_input = "li".to_string();
    st.candidate_limit = 10;
    // 向下箭头：若无守卫会触发 expand_candidates → build_candidates 重建（headless 得空），
    // 过滤结果被清空。
    let _ = c.handle_aux_code_key(&mut st, &key(keymap::VK_DOWN, 0));
    let texts: Vec<&str> = st.candidates.iter().map(|c| c.text.as_str()).collect();
    assert_eq!(texts, vec!["李", "樱"], "↓ 不得丢失辅助码过滤结果");
    assert_eq!(st.active, Some(ModeKind::AuxCode));
}

// ───────────────────────── 开关三态（出厂关 / 方案覆盖两个方向）─────────────────────────

/// 出厂默认关闭：方案配了 `files`、也绑了触发键，但没写 `enabled`，由全局基线
/// `[schema.pinyin.aux_code].enabled = false` 说了算。「方案里配了码表」不构成开启意图。
#[test]
fn aux_code_disabled_by_default_does_not_enter() {
    let dir = data_dir_with_aux_enabled("default_off", None);
    let c = coord_with_data("default_off", dir);
    let mut st = seed_composition(&c);
    let act = c.enter_aux_code(&mut st, keymap::VK_BACKTICK);
    assert!(
        act.is_none(),
        "出厂未启用时门卫必须不吞键（落普通标点流程）"
    );
    assert_eq!(st.active, None, "不得进入辅助码模式");
    assert!(st.aux_code.is_none());
    assert_eq!(st.candidates.len(), 3, "候选原封不动（未被 take 走）");
    assert_eq!(st.preedit, "li", "组合区原封不动");
}

/// Tab 这类非符号键靠 `[session_actions]` 进辅助码：`key_actions` 解析不出 `tab`
/// （`key_action_name_to_vk` 只认符号键、字母 z 与四个修饰键），写进去会被静默丢弃。
#[test]
fn session_action_tab_enters_aux_code() {
    let c = coord_with_data_cfg("sess_tab", data_dir_with_aux("sess_tab"), |cfg| {
        cfg.keys
            .session_actions
            .insert("tab".to_string(), "aux_code".to_string());
    });
    let mut st = seed_composition(&c);
    let act = c.apply_session_action(&mut st, &key(keymap::VK_TAB, 0), true);
    assert!(act.is_some(), "有候选时 Tab 应进辅助码");
    assert_eq!(st.active, Some(ModeKind::AuxCode));
    assert!(st.aux_code.is_some(), "overlay 三件套应已建立");
}

/// 无候选时必须放行，否则空闲按 Tab 就打不出制表符。守的是 `requires_candidates()` 通用闸门，
/// 有人给 `AuxCode` 加特例（如照 `Cancel` 放宽到「有会话即可」）这条就红。
#[test]
fn session_action_tab_passes_through_without_candidates() {
    let c = coord_with_data_cfg("sess_tab_idle", data_dir_with_aux("sess_tab_idle"), |cfg| {
        cfg.keys
            .session_actions
            .insert("tab".to_string(), "aux_code".to_string());
    });
    let mut st = c.state.lock().unwrap();
    st.chinese_mode = true;
    // 空闲态：无缓冲、无候选。
    let act = c.apply_session_action(&mut st, &key(keymap::VK_TAB, 0), true);
    assert!(act.is_none(), "无候选时不得吞键——Tab 要还给宿主");
    assert_eq!(st.active, None);
}

/// 触发键在辅助码态里再按一次 = 退出，绝不重入（重入会让 `preedit_prefix` 每次多 4 个空格）。
/// 判据取「组合区长度不增长」：重入后 `active` 仍是 AuxCode，只看它测不出。
#[test]
fn trigger_key_exits_instead_of_reentering() {
    let c = coord_with_data_cfg("sess_reenter", data_dir_with_aux("sess_reenter"), |cfg| {
        cfg.keys
            .session_actions
            .insert("tab".to_string(), "aux_code".to_string());
    });
    let mut st = seed_composition(&c);
    let tab = key(keymap::VK_TAB, 0);
    c.apply_session_action(&mut st, &tab, true).expect("应进入");
    assert_eq!(st.active, Some(ModeKind::AuxCode));
    let after_enter = st.preedit.clone();
    assert_eq!(after_enter, "li    ", "进入后 = 拼音 + 4 空格");

    // 再按一次：退出并还原拼音组合，而不是把前缀又拼一遍。
    let act = c.handle_aux_code_key(&mut st, &tab);
    assert!(matches!(act, KeyAction::UpdateComposition { .. }));
    assert_eq!(st.active, None, "触发键再按一次应退出辅助码");
    assert!(st.aux_code.is_none(), "overlay 三件套应整体销毁");
    assert_eq!(st.preedit, "li", "组合区应还原成拼音，不带任何残留空格");
    assert_eq!(st.candidates.len(), 3, "候选应还原");
}

/// 再按触发键不能上屏：少了「触发键退出」这一支，键会落到兜底臂上屏高亮候选。
#[test]
fn trigger_key_does_not_commit_on_exit() {
    let c = coord_with_data_cfg("sess_nocommit", data_dir_with_aux("sess_nocommit"), |cfg| {
        cfg.keys
            .session_actions
            .insert("tab".to_string(), "aux_code".to_string());
    });
    let mut st = seed_composition(&c);
    let tab = key(keymap::VK_TAB, 0);
    c.apply_session_action(&mut st, &tab, true).expect("应进入");
    let _ = c.handle_aux_code_key(&mut st, &tab);
    // 判据取「候选还在、缓冲还在」而不是看返回的 KeyAction 变体：兜底臂走
    // `commit_selected`，它在部分消费时同样返回 `UpdateComposition`，按变体断言测不出来。
    assert_eq!(st.candidates.len(), 3, "退出不得吃掉候选（那意味着上屏了）");
    assert_eq!(st.input_buffer, "li", "退出不得消费编码缓冲");
    assert!(st.committed_text.is_empty(), "不该有待上屏文本");
}

/// 辅助码态里字母恒是码元，即便它在 `key_actions` 里绑了 `aux_code`。
///
/// 会话表那侧天然排除（`z` 标着 `printable: true`，辅助码态取 `include_printable = false`）；
/// `key_actions` 没有 `printable` 维度，得显式排，否则配了 `z = "aux_code"` 的用户在辅助码里
/// 打不出笔画码的「折」。进入侧不管，拼音里绑了 z 是用户自己的配置。
#[test]
fn letter_stays_code_input_even_when_bound_as_trigger() {
    let c = coord_with_data_cfg("sess_zletter", data_dir_with_aux("sess_zletter"), |cfg| {
        cfg.keys
            .key_actions
            .insert("z".to_string(), "aux_code".to_string());
    });
    let mut st = seed_composition(&c);
    c.enter_aux_code(&mut st, keymap::VK_BACKTICK)
        .expect("反引号应进得去");
    let _ = c.handle_aux_code_key(&mut st, &key(vk_letter('Z'), 0));
    assert_eq!(
        st.active,
        Some(ModeKind::AuxCode),
        "z 是码元，不得当成退出键"
    );
    assert_eq!(
        st.aux_code.as_ref().map(|o| o.session.buffer()),
        Some("z"),
        "z 应累积进辅助码缓冲"
    );
}

/// 临拼以外的 overlay 模式里，触发键不得把辅助码套进来。
///
/// `handle_candidate_nav` 被五个 overlay 共用，少了 `enter_aux_code` 的守卫，Tab 会夺走那个模式
/// 的候选。`TempPinyin` 产出的正是拼音候选，单独放行（见 `enters_from_temp_pinyin_*`）。
#[test]
fn enter_is_refused_from_non_pinyin_overlay_modes() {
    let c = coord_with("overlay_guard");
    let mut st = seed_composition(&c);
    for mode in [
        ModeKind::TempEnglish,
        ModeKind::AuxCode,
        ModeKind::Url,
        ModeKind::Special(0),
        ModeKind::Mix(0),
    ] {
        st.active = Some(mode);
        assert!(
            c.enter_aux_code(&mut st, keymap::VK_TAB).is_none(),
            "{mode:?} 态下不得进入辅助码"
        );
        assert_eq!(st.active, Some(mode), "被拒时不得改动 active");
        assert_eq!(st.candidates.len(), 3, "被拒时候选必须原封不动");
        assert_eq!(st.preedit, "li", "被拒时组合区必须原封不动");
    }
}

/// 装填「临时拼音组合中」的初始状态：码在 `temp_pinyin_buffer`（不是 `input_buffer`），
/// 方案指向 fixture 的 pinyin，引导前缀 `z`。与 `seed_composition` 是同一批候选。
fn seed_temp_pinyin(c: &Arc<Coordinator>) -> std::sync::MutexGuard<'_, crate::coordinator::State> {
    let mut st = c.state.lock().unwrap();
    st.chinese_mode = true;
    st.active = Some(ModeKind::TempPinyin);
    st.temp_pinyin_schema = "pinyin".to_string();
    st.temp_pinyin_buffer = "li".to_string();
    st.temp_pinyin_cursor = 2;
    st.temp_pinyin_prefix = "z".to_string();
    st.candidates = vec![cand("李"), cand("樱"), cand("河")];
    st.selected_index = 0;
    st.current_page = 0;
    st.preedit = "zli".to_string();
    st
}

/// 在 `data_dir_with_aux` 之上再放一个码表方案，供「活跃方案 ≠ 临拼目标方案」的用例。
/// 两者相同时 `bound_action_for` 与 `bound_action_in_schema` 答案相同，变异不会变红。
/// 这个方案刻意不写 `[key_actions]`：绑定只存在于 pinyin 那份里。
fn data_dir_with_codetable_active(tag: &str) -> std::path::PathBuf {
    let dir = data_dir_with_aux(tag);
    std::fs::write(
        dir.join("schemas").join("wb.schema.toml"),
        "[schema]\nid = \"wb\"\nname = \"wb\"\n[engine]\ntype = \"codetable\"\n",
    )
    .unwrap();
    dir
}

/// 临拼里按目标方案（拼音/双拼）`[key_actions]` 声明的辅助码触发键即可进入。
///
/// 如五笔主方案 + z 引导临拼，`backtick = "aux_code"` 写在出厂 shuangpin.schema.toml 里；
/// 按活跃方案取则永远进不去。辅助码是「编码类」动词，按产出候选的方案取（见
/// docs/design/key-resolver-unification.md §4.4），整张表并不随目标方案走。
#[test]
fn temp_pinyin_enters_aux_by_target_schema_binding() {
    let dir = data_dir_with_codetable_active("tp_aux_bind");
    let c = coord_with_data_cfg("tp_aux_bind", dir, |cfg| {
        cfg.schema.active = "wb".to_string();
        cfg.schema.available = vec!["wb".to_string(), "pinyin".to_string()];
    });
    let mut st = seed_temp_pinyin(&c);
    // 前置条件：活跃方案（wb）不认这条绑定——否则本用例测的是回落而不是目标方案。
    assert_ne!(
        c.bound_action_for(keymap::VK_BACKTICK),
        Some(wind_config::BoundAction::AuxCode),
        "前置：活跃方案不该有 aux_code 绑定"
    );
    let act = c.handle_temp_pinyin_key(&mut st, &key(keymap::VK_BACKTICK, 0));
    assert_eq!(
        st.active,
        Some(ModeKind::AuxCode),
        "应按临拼目标方案的 [key_actions] 进入辅助码，实际: {act:?}"
    );
    assert_eq!(
        st.aux_code.as_ref().and_then(|o| o.origin),
        Some(ModeKind::TempPinyin)
    );
}

/// 字母键不参与目标方案的辅助码触发判定，与 `aux_code_key_role` 的第一道守卫同源；
/// 否则配过 `z = "aux_code"` 的用户在临拼里打不出 z。
#[test]
fn temp_pinyin_letter_not_taken_as_aux_trigger() {
    let dir = data_dir_with_codetable_active("tp_aux_letter");
    // 把 z 也绑成 aux_code：字母守卫若缺失，下面按 z 就会进辅助码而不是累积拼音。
    std::fs::write(
        dir.join("schemas").join("pinyin.schema.toml"),
        "[schema]\nid = \"pinyin\"\nname = \"pinyin\"\n\
             [engine]\ntype = \"pinyin\"\n\
             [engine.aux_code]\nfiles = [\"aux_code/flypy_test.txt\"]\nenabled = true\n\
             [key_actions]\nz = \"aux_code\"\n",
    )
    .unwrap();
    let c = coord_with_data_cfg("tp_aux_letter", dir, |cfg| {
        cfg.schema.active = "wb".to_string();
        cfg.schema.available = vec!["wb".to_string(), "pinyin".to_string()];
    });
    let mut st = seed_temp_pinyin(&c);
    c.handle_temp_pinyin_key(&mut st, &key(vk_letter('Z'), 0));
    assert_eq!(
        st.active,
        Some(ModeKind::TempPinyin),
        "z 是码元，不得进辅助码"
    );
    assert_eq!(st.temp_pinyin_buffer, "liz", "z 应累积进临拼缓冲");
}

/// 临拼态可进辅助码，并记住来源；筛选照常作用在临拼候选上（临拼产出的就是拼音候选）。
#[test]
fn enters_from_temp_pinyin_and_filters() {
    let c = coord_with("from_temp");
    let mut st = seed_temp_pinyin(&c);
    c.enter_aux_code(&mut st, keymap::VK_BACKTICK)
        .expect("临拼有候选时应能进入辅助码");
    assert_eq!(st.active, Some(ModeKind::AuxCode));
    assert_eq!(
        st.aux_code.as_ref().and_then(|o| o.origin),
        Some(ModeKind::TempPinyin),
        "来源必须记下来——退出/上屏/切英文全靠它分流"
    );
    // 显示基线取的是临拼的组合区（含引导前缀 z），不是空的 input_buffer。
    assert_eq!(
        st.aux_code.as_ref().map(|o| o.preedit_base.as_str()),
        Some("zli"),
        "显示基线应是临拼组合区"
    );
    // 打 m → 李(mz)/樱(my) 命中，河(sk) 被滤。
    let _ = c.handle_aux_code_key(&mut st, &key(vk_letter('M'), 0));
    let texts: Vec<&str> = st.candidates.iter().map(|c| c.text.as_str()).collect();
    assert_eq!(texts, vec!["李", "樱"], "辅助码筛选应作用在临拼候选上");
}

/// 退出辅助码回到临拼，而不是掉进「什么模式都不在」——否则组合区与 `temp_pinyin_buffer`
/// 还在，下一个字母会被当成新一轮主方案的码。
#[test]
fn exits_back_to_temp_pinyin() {
    let c = coord_with("exit_temp");
    let mut st = seed_temp_pinyin(&c);
    c.enter_aux_code(&mut st, keymap::VK_BACKTICK).unwrap();
    c.exit_aux_code(&mut st);
    assert_eq!(st.active, Some(ModeKind::TempPinyin), "应退回临拼");
    assert!(st.aux_code.is_none());
    assert_eq!(st.candidates.len(), 3, "候选应还原为进入前的快照");
    assert_eq!(st.preedit, "zli", "组合区应还原成临拼形态");
    assert_eq!(st.temp_pinyin_buffer, "li", "临拼缓冲全程不该被动过");
}

/// 主输入路来源仍退到 `None`——`origin` 的 `None` 分支必须与改动前逐位等价。
#[test]
fn exits_to_main_path_when_origin_is_none() {
    let c = coord_with("exit_main");
    let mut st = seed_composition(&c);
    c.enter_aux_code(&mut st, keymap::VK_BACKTICK).unwrap();
    assert_eq!(st.aux_code.as_ref().and_then(|o| o.origin), None);
    c.exit_aux_code(&mut st);
    assert_eq!(st.active, None, "主输入路来源退出后不应停在任何模式里");
    assert_eq!(st.preedit, "li");
}

/// 「部分消费 vs 完整消费」的判据必须按来源缓冲取。写死 `input_buffer` 的话，从临拼进来的
/// 会话恒判完整消费，分步组句选第一个词就丢掉剩余拼音。同一个 `State` 下两种来源必须给出不同答案。
#[test]
fn source_buffer_follows_origin() {
    let c = coord_with("src_buf");
    let mut st = seed_temp_pinyin(&c);
    assert!(st.input_buffer.is_empty(), "前置：临拼态主缓冲为空");
    c.enter_aux_code(&mut st, keymap::VK_BACKTICK).unwrap();
    assert_eq!(
        c.aux_code_source_buffer(&st),
        "li",
        "临拼来源应取 temp_pinyin_buffer"
    );
    // 把来源改成主输入路，同一个 State 下答案必须翻转（锁住「不是恒取某一个」）。
    st.aux_code.as_mut().unwrap().origin = None;
    assert_eq!(
        c.aux_code_source_buffer(&st),
        "",
        "主输入路来源应取 input_buffer（此处为空）"
    );
}

/// 两张表的动词写法必须逐字一致，否则配置从一张表挪到另一张就静默失效。共键形态
/// （`aux_code:page_next`）只有 `session_actions` 有，不算分叉：`key_actions` 解析不出翻页键。
#[test]
fn aux_code_verb_spelling_matches_across_both_tables() {
    use wind_config::{AuxCodeShare, BoundAction, SessionAction};
    assert_eq!(BoundAction::parse("aux_code"), BoundAction::AuxCode);
    assert_eq!(
        SessionAction::parse("aux_code"),
        SessionAction::AuxCode(AuxCodeShare::Solo)
    );
    // 写回也要能读回来（Display 与 parse 互逆）。
    assert_eq!(
        SessionAction::AuxCode(AuxCodeShare::Solo).to_string(),
        "aux_code"
    );
}

/// tri-state 覆盖方向一：全局关 + 方案显式开 → 进得去。
/// 这正是「只在双拼开、全拼不动」的落地形态。
#[test]
fn aux_code_schema_override_enables_over_global_off() {
    let c = coord_with("schema_on"); // fixture 方案写了 enabled = true
    assert!(
        !Config::default().schema.pinyin.aux_code.enabled,
        "前提：全局基线出厂为关，本用例才证明得了方案覆盖生效"
    );
    let mut st = seed_composition(&c);
    let act = c.enter_aux_code(&mut st, keymap::VK_BACKTICK);
    assert!(act.is_some(), "方案 enabled = true 应覆盖全局的关");
    assert_eq!(st.active, Some(ModeKind::AuxCode));
}

/// tri-state 覆盖方向二：全局开 + 方案显式关 → 仍进不去。只测方向一会漏掉
/// 「覆盖只实现了 `Some(true)` 分支」的半截实现。
#[test]
fn aux_code_schema_override_disables_over_global_on() {
    let dir = data_dir_with_aux_enabled("schema_off", Some(false));
    let path = std::env::temp_dir().join("wind_aux_code_schema_off.redb");
    let _ = std::fs::remove_file(&path);
    let store = Arc::new(Store::open(&path).unwrap());
    let mut cfg = Config::default();
    cfg.schema.active = "pinyin".to_string();
    cfg.schema.pinyin.aux_code.enabled = true; // 全局开
    let c = Coordinator::new_headless_with_store(cfg, Some(&dir), store);
    let mut st = seed_composition(&c);
    let act = c.enter_aux_code(&mut st, keymap::VK_BACKTICK);
    assert!(act.is_none(), "方案 enabled = false 应覆盖全局的开");
    assert_eq!(st.active, None);
}

// ───────────────────────── 缺陷回归 ─────────────────────────

/// 鼠标点击选词必须与键盘选词走同一条 `commit_selected` 路径。
///
/// `select_candidate_at` 若以 `state.active.is_none()` 区分主输入路，辅助码会落进 overlay 分支：
/// 分步组句点第一个字就丢掉剩余拼音，且 `state.aux_code` 不被清理（三件套「同生共死」被打破）。
#[test]
fn mouse_click_commit_keeps_stepwise_conversion() {
    let c = coord_with("mouse_partial");
    {
        let mut st = seed_composition(&c);
        st.candidates[0].consumed_length = 1; // 李 只消费 li 的前 1 码
        let _ = c.enter_aux_code(&mut st, keymap::VK_BACKTICK);
        let _ = c.handle_aux_code_key(&mut st, &key(vk_letter('M'), 0)); // 筛 m → 李/樱
        assert_eq!(st.active, Some(ModeKind::AuxCode));
    } // select_candidate_at 自己取锁，必须先放
    let _ = c.select_candidate_at(0); // 鼠标点第一个候选（李）
    let st = c.state.lock().unwrap();
    assert_eq!(
        st.active,
        Some(ModeKind::AuxCode),
        "部分消费：鼠标点选也应留在辅助码模式继续筛下一段（同键盘）"
    );
    assert_eq!(st.committed_text, "李", "逐步转换：已转换前缀并入");
    assert_eq!(st.input_buffer, "i", "剩余编码必须保留，不得被 clear 掉");
    assert!(st.aux_code.is_some(), "overlay 已按新候选重建，不是残留");
    assert!(
        st.aux_code.as_ref().unwrap().session.is_empty(),
        "重建会话的辅助码缓冲应清空，可继续输入下一段"
    );
}

/// 回归：鼠标点选完整消费的候选 → 退出辅助码且 overlay 必须清干净。
#[test]
fn mouse_click_full_commit_clears_overlay() {
    let c = coord_with("mouse_full");
    {
        let mut st = seed_composition(&c); // consumed_length 全 0 = 整串消费
        let _ = c.enter_aux_code(&mut st, keymap::VK_BACKTICK);
    }
    let _ = c.select_candidate_at(0);
    let st = c.state.lock().unwrap();
    assert_eq!(st.active, None, "整串消费后退出辅助码");
    assert!(
        st.aux_code.is_none(),
        "overlay 必须随模式一起销毁——否则候选快照会一直挂着"
    );
}

/// 辅助码态下切英文，`commit_on_switch` 开启时必须上屏拼音原码。
///
/// 辅助码是唯一不清空 `input_buffer` 的独占模式；`take_input_on_mode_switch` 的独占分支若假定
/// 「独占模式下 input_buffer 必为空」，待上屏的拼音码会被静默丢弃。
#[test]
fn mode_switch_to_english_commits_pinyin_code() {
    let c = coord_with("switch_en");
    let mut st = seed_composition(&c);
    let _ = c.enter_aux_code(&mut st, keymap::VK_BACKTICK);
    let _ = c.handle_aux_code_key(&mut st, &key(vk_letter('M'), 0));
    assert_eq!(st.active, Some(ModeKind::AuxCode));
    // 切英文（commit_on_switch 出厂即开，见 keys.commit_on_switch）。
    let text = c.take_input_on_mode_switch(&mut st, false);
    assert_eq!(text, "li", "待上屏的拼音原码不得因为在辅助码模式里而丢失");
    assert_eq!(st.active, None, "独占模式一并复位");
    assert!(st.aux_code.is_none());
}

/// 进辅助码不得改变候选窗的分页档位：`layout::intent_for` 的 AuxCode 臂返回 `None`
/// （沿用主路径呈现），`per_page` 不能按 `active.is_some()` 切到扩展档。
///
/// 出厂 `per_page_extended = 0` 时两档相同、缺陷不可见，fixture 必须显式配上扩展档。
#[test]
fn aux_code_keeps_main_path_per_page() {
    let c = coord_with_data_cfg("per_page", data_dir_with_aux("per_page"), |cfg| {
        cfg.ui.candidate.per_page = 5;
        cfg.ui.candidate.per_page_extended = 9;
    });
    assert_eq!(c.per_page(None), 5, "主输入路用 per_page");
    assert_eq!(
        c.per_page(Some(ModeKind::TempPinyin)),
        9,
        "真正的 overlay 模式（临拼另起一套候选）才用扩展档"
    );
    assert_eq!(
        c.per_page(Some(ModeKind::AuxCode)),
        5,
        "辅助码只是把主路径候选筛了一轮，分页档位须与主输入路一致"
    );

    // 端到端：同一批候选，进模式前后总页数不得跳变（12 条 → 5 档 3 页 / 9 档 2 页）。
    let mut st = seed_composition(&c);
    st.candidates = (0..12).map(|i| cand(&format!("字{i}"))).collect();
    let pages_before = c.total_pages(&st);
    assert_eq!(pages_before, 3, "前置：主路径 12 条 / 每页 5 = 3 页");
    let _ = c
        .enter_aux_code(&mut st, keymap::VK_BACKTICK)
        .expect("有候选应进入");
    assert_eq!(
        c.total_pages(&st),
        pages_before,
        "进辅助码的瞬间总页数不得跳变"
    );
}

/// 辅助码态下末页翻页不得触发「检索范围临时放宽」：放宽会经 `build_candidates` 重建整池候选，
/// 把辅助码筛出的结果冲掉。
///
/// 判据只能取候选列表本身：放宽后若无被滤候选会自行撤销，两条路径的返回值都是 false。
#[test]
fn aux_code_does_not_relax_scope_on_page_end() {
    let c = coord_with("relax");
    let mut st = seed_composition(&c);
    let _ = c.enter_aux_code(&mut st, keymap::VK_BACKTICK);
    let _ = c.handle_aux_code_key(&mut st, &key(vk_letter('M'), 0));
    let kept: Vec<&str> = st.candidates.iter().map(|c| c.text.as_str()).collect();
    assert_eq!(kept, vec!["李", "樱"], "前置：m 筛出 李/樱");

    let changed = c.try_relax_scope_on_page_end(&mut st);

    assert!(!changed, "辅助码态不放宽（无变化 → 上层不必重绘）");
    assert!(!st.scope_relaxed, "更不得留下一个影响后续按键的放宽态");
    let kept: Vec<&str> = st.candidates.iter().map(|c| c.text.as_str()).collect();
    assert_eq!(kept, vec!["李", "樱"], "辅助码的筛选结果必须原样保留");
}

// ─────────────── 与翻页共键（`aux_code:page_next`） ────────────────

/// 装填「多页候选」的初始状态：12 条 > 出厂每页 9 条 ⇒ 2 页，翻页才有可观测效果。
fn seed_multi_page(c: &Arc<Coordinator>) -> std::sync::MutexGuard<'_, crate::coordinator::State> {
    let mut st = seed_composition(c);
    st.candidates = (0..12).map(|i| cand(&format!("候选{i}"))).collect();
    st
}

fn coord_with_share_key(tag: &str, enabled: Option<bool>) -> Arc<Coordinator> {
    coord_with_data_cfg(tag, data_dir_with_aux_enabled(tag, enabled), |cfg| {
        cfg.keys
            .session_actions
            .insert("tab".to_string(), "aux_code:page_next".to_string());
    })
}

/// 共键先试辅助码、不先翻页：首按 Tab 停在第一页，首选不会被翻走。
///
/// 盯的是「进入后停在第几页」这个契约：进入必经 `reset_candidate_view` 把页码归零，所以
/// 「先翻页但不保留页码」与「不翻页」在这里不可区分；「先翻页」形态必须再补一道保存/恢复
/// `current_page` 才成立，那时本用例会红。
#[test]
fn share_key_enters_aux_without_paging() {
    let c = coord_with_share_key("share_enter", Some(true));
    let mut st = seed_multi_page(&c);
    let act = c
        .apply_session_action(&mut st, &key(keymap::VK_TAB, 0), true)
        .expect("有候选且辅助码可用 ⇒ 进辅助码");
    assert_eq!(st.active, Some(ModeKind::AuxCode));
    assert_eq!(st.current_page, 0, "进入时不得先翻页");
    assert_eq!(st.selected_index, 0, "高亮仍在首选");
    assert_eq!(st.preedit, "li    ", "组合区 = 拼音 + 4 空格");
    assert!(matches!(act, KeyAction::UpdateComposition { .. }));
}

/// 辅助码态内再按共键 = 翻页，不退出。走 `handle_aux_code_key` 以连 `is_aux_code_trigger`
/// 一起测——它若把共键也认成退出键，就永远翻不到第二页。
#[test]
fn share_key_pages_inside_aux_mode() {
    let c = coord_with_share_key("share_page", Some(true));
    let mut st = seed_multi_page(&c);
    c.apply_session_action(&mut st, &key(keymap::VK_TAB, 0), true)
        .expect("应进入辅助码");
    assert_eq!(st.current_page, 0);

    let act = c.handle_aux_code_key(&mut st, &key(keymap::VK_TAB, 0));

    assert_eq!(st.active, Some(ModeKind::AuxCode), "模式内按共键不得退出");
    assert_eq!(st.current_page, 1, "应翻到第二页");
    assert!(st.aux_code.is_some(), "overlay 三件套仍在");
    assert_eq!(st.candidates.len(), 12, "翻页不得动候选");
    assert!(matches!(act, KeyAction::Consumed));
}

/// 辅助码未启用 ⇒ 门卫拒 ⇒ 共键退化成纯翻页键（而不是哑键）。出厂即未启用，这是多数用户的第一手体验。
#[test]
fn share_key_degrades_to_paging_when_aux_disabled() {
    let c = coord_with_share_key("share_off", Some(false));
    let mut st = seed_multi_page(&c);
    let act = c
        .apply_session_action(&mut st, &key(keymap::VK_TAB, 0), true)
        .expect("降级为翻页 ⇒ 仍然消费按键");
    assert_eq!(st.active, None, "未启用不得进辅助码");
    assert!(st.aux_code.is_none(), "更不得建 overlay");
    assert_eq!(st.current_page, 1, "应当作纯翻页键");
    assert!(matches!(act, KeyAction::Consumed));
}

/// 无候选（空闲）⇒ 两个成员都 `requires_candidates` ⇒ 整个动词放行，Tab 还给宿主。
/// 判据取「返回 None」：这是「不吞键」的唯一表达。
#[test]
fn share_key_yields_when_no_candidates() {
    let c = coord_with_share_key("share_idle", Some(true));
    let mut st = c.state.lock().unwrap();
    st.chinese_mode = true;
    assert!(
        c.apply_session_action(&mut st, &key(keymap::VK_TAB, 0), true)
            .is_none(),
        "无候选时共键必须放行（空闲按 Tab 仍是宿主的制表符）"
    );
    assert_eq!(st.active, None);
}

/// 已在临拼以外的 overlay 模式里 ⇒ `enter_aux_code` 的守卫拒 ⇒ 只翻页，不能把主候选夺过来筛。
/// 代表模式取 `TempEnglish`；临拼的语义由 `share_key_enters_aux_from_temp_pinyin` 单独锁。
#[test]
fn share_key_only_pages_in_other_overlay_modes() {
    let c = coord_with_share_key("share_overlay", Some(true));
    let mut st = seed_multi_page(&c);
    st.active = Some(ModeKind::TempEnglish);
    let act = c
        .apply_session_action(&mut st, &key(keymap::VK_TAB, 0), true)
        .expect("翻页 ⇒ 消费");
    assert_eq!(st.active, Some(ModeKind::TempEnglish), "不得改动所在模式");
    assert!(st.aux_code.is_none());
    assert_eq!(st.current_page, 1);
    assert!(matches!(act, KeyAction::Consumed));
}

/// 共键在临拼里与主输入路同语义：能进辅助码就进，进不去才翻页——否则同一功能两个入口两种结果。
#[test]
fn share_key_enters_aux_from_temp_pinyin() {
    let c = coord_with_share_key("share_temp", Some(true));
    let mut st = seed_multi_page(&c);
    st.active = Some(ModeKind::TempPinyin);
    st.temp_pinyin_schema = "pinyin".to_string();
    st.temp_pinyin_buffer = "li".to_string();
    c.apply_session_action(&mut st, &key(keymap::VK_TAB, 0), true)
        .expect("进入辅助码 ⇒ 消费");
    assert_eq!(
        st.active,
        Some(ModeKind::AuxCode),
        "共键应把临拼候选接进辅助码"
    );
    assert_eq!(
        st.aux_code.as_ref().and_then(|o| o.origin),
        Some(ModeKind::TempPinyin),
        "来源须记成临拼"
    );
}

/// 专用触发键只配在 `session_actions` 的符号键上时，模式内再按必须正常退出，且绝不上屏。
///
/// 符号键一律 `printable = true`，按辅助码态的 `include_printable = false` 去查会查不到，键落到
/// 兜底臂上屏首选；故 `aux_code_key_role` 取 `include_printable = true`（字母码元已单独排除）。
/// fixture 刻意用没在 `key_actions` 里绑过的 `/`，否则测的是 key_actions 那条路。
/// 判据取「候选与缓冲原封不动」：兜底臂部分消费时同样返回 `UpdateComposition`。
#[test]
fn solo_trigger_on_symbol_key_exits_without_commit() {
    let c = coord_with_data_cfg("solo_symbol", data_dir_with_aux("solo_symbol"), |cfg| {
        cfg.keys
            .session_actions
            .insert("slash".to_string(), "aux_code".to_string());
    });
    let mut st = seed_composition(&c);
    let slash = key(keymap::VK_SLASH, 0);
    assert_ne!(
        c.bound_action_for(keymap::VK_SLASH),
        Some(wind_config::BoundAction::AuxCode),
        "前提：这个键在 key_actions 里没有绑定，本用例才测得到会话表那条路"
    );
    c.apply_session_action(&mut st, &slash, true)
        .expect("应进入辅助码");

    let act = c.handle_aux_code_key(&mut st, &slash);

    assert_eq!(st.active, None, "专用触发键再按一次应退出");
    assert_eq!(st.candidates.len(), 3, "退出不得吃掉候选（那意味着上屏了）");
    assert_eq!(st.input_buffer, "li", "退出不得消费编码缓冲");
    assert!(st.committed_text.is_empty(), "不该有待上屏文本");
    assert!(matches!(act, KeyAction::UpdateComposition { .. }));
}

/// 同一个键两张表各有一份、且形态不同时，退出判定必须跟随分派优先级（会话表先）。
///
/// 双拼出厂就带 `[key_actions] backtick = "aux_code"`，用户在 `session_actions` 把反引号改配成
/// 共键即会撞上。若「任一表命中即算触发键」，进入走共键、退出被 key_actions 认走，第二页永远到不了。
#[test]
fn share_key_wins_over_key_actions_binding_on_same_key() {
    let c = coord_with_data_cfg(
        "share_bothtables",
        data_dir_with_aux("share_bothtables"),
        |cfg| {
            cfg.keys
                .session_actions
                .insert("backtick".to_string(), "aux_code:page_next".to_string());
        },
    );
    let mut st = seed_multi_page(&c);
    let bt = key(keymap::VK_BACKTICK, 0);
    // 前提断言：fixture 方案的 [key_actions] 里确实还绑着 backtick = "aux_code"。
    // 少了它本用例就退化成普通共键测试，测不到「两张表打架」这件事。
    assert_eq!(
        c.bound_action_for(keymap::VK_BACKTICK),
        Some(wind_config::BoundAction::AuxCode),
        "前提：key_actions 那份绑定仍在"
    );

    c.apply_session_action(&mut st, &bt, true)
        .expect("共键应进辅助码");
    assert_eq!(st.active, Some(ModeKind::AuxCode));

    let act = c.handle_aux_code_key(&mut st, &bt);

    assert_eq!(
        st.active,
        Some(ModeKind::AuxCode),
        "会话表配的是共键 ⇒ key_actions 那份不得把它认成退出键"
    );
    assert_eq!(st.current_page, 1, "应当翻页");
    assert!(matches!(act, KeyAction::Consumed));
}

/// 动词写法：`aux_code` / `aux_code:page_next` 与 `Display` 互逆；写错的参数落 `None`，
/// 由 `parse_checked` 在加载期告警，而不是静默降级成专用触发键。
#[test]
fn share_verb_parse_and_display_roundtrip() {
    use wind_config::{AuxCodeShare, SessionAction};
    assert_eq!(
        SessionAction::parse("aux_code"),
        SessionAction::AuxCode(AuxCodeShare::Solo)
    );
    assert_eq!(
        SessionAction::parse(" AUX_CODE:PAGE_NEXT "),
        SessionAction::AuxCode(AuxCodeShare::PageNext)
    );
    assert_eq!(
        SessionAction::AuxCode(AuxCodeShare::PageNext).to_string(),
        "aux_code:page_next"
    );
    assert_eq!(
        SessionAction::AuxCode(AuxCodeShare::Solo).to_string(),
        "aux_code"
    );
    assert_eq!(
        SessionAction::parse("aux_code:page_prev"),
        SessionAction::None
    );
    assert_eq!(SessionAction::parse_checked("aux_code:page_prev"), None);
}

/// 自造码表方案（工=a/aaaa、攻=atyy、公=wcu），供 `schema:<id>` 引用。id 由调用方给且每条
/// 用例不同（含进程号）：反查索引的磁盘缓存按方案 id 落在共享目录，共用会互相覆盖。
fn write_wbx(schemas: &std::path::Path, id: &str) {
    std::fs::create_dir_all(schemas.join(id)).unwrap();
    std::fs::write(
        schemas.join(format!("{id}.schema.toml")),
        format!(
            "[schema]\nid = \"{id}\"\nname = \"测五\"\n[engine]\ntype = \"codetable\"\n\
                 [engine.codetable]\nmax_code_length = 4\n\
                 [[dictionaries]]\nid = \"{id}_main\"\npath = \"{id}/{id}.dict.yaml\"\n\
                 type = \"rime_codetable\"\ndefault = true\n"
        ),
    )
    .unwrap();
    std::fs::write(
        schemas.join(format!("{id}/{id}.dict.yaml")),
        format!(
            "---\nname: {id}\nversion: \"1\"\ncolumns:\n  - code\n  - text\n  - weight\n...\n\
                 a\t工\t100\naaaa\t工\t90\natyy\t攻\t80\nwcu\t公\t80\n"
        ),
    )
    .unwrap();
}

/// 本进程内唯一的码表方案 id（见 [`write_wbx`]）。
fn wbx_id(tag: &str) -> String {
    format!("zz_ax_{tag}_{}", std::process::id())
}

/// 用例结束（含 panic）时清掉夹具目录与码表方案在共享缓存根下的产物（`<cache>/<id>/`）。
/// id 带进程号，不清的话每跑一次就在真实缓存目录里多留一个 `zz_ax_*`，无上限增长。
struct Cleanup {
    id: String,
    dir: std::path::PathBuf,
}

impl Drop for Cleanup {
    fn drop(&mut self) {
        if let Some(cache) = Config::cache_dir() {
            let _ = std::fs::remove_dir_all(cache.join(&self.id));
        }
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// pinyin 方案的 `files` 由调用方给（TOML 数组字面量），并带上 `wbx_id` 方案与 `flypy_test.txt`。
/// 返回的 [`Cleanup`] 要一直持有到用例结束。
fn data_dir_with_files(tag: &str, files: &str, wbx_id: &str) -> (std::path::PathBuf, Cleanup) {
    let dir = std::env::temp_dir().join(format!("wind_aux_code_src_{tag}_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let guard = Cleanup {
        id: wbx_id.to_string(),
        dir: dir.clone(),
    };
    let schemas = dir.join("schemas");
    std::fs::create_dir_all(schemas.join("aux_code")).unwrap();
    std::fs::write(
        schemas.join("pinyin.schema.toml"),
        format!(
            "[schema]\nid = \"pinyin\"\nname = \"pinyin\"\n[engine]\ntype = \"pinyin\"\n\
                 [engine.aux_code]\nfiles = {files}\nenabled = true\n\
                 [key_actions]\nbacktick = \"aux_code\"\n"
        ),
    )
    .unwrap();
    std::fs::write(
        schemas.join("aux_code/flypy_test.txt"),
        "李=mz\n樱=my\n河=sk\n",
    )
    .unwrap();
    std::fs::write(schemas.join("aux_code/other.txt"), "李=qq\n樱=qq\n河=qq\n").unwrap();
    write_wbx(&schemas, wbx_id);
    (dir, guard)
}

fn seed<'a>(
    c: &'a Arc<Coordinator>,
    texts: &[&str],
) -> std::sync::MutexGuard<'a, crate::coordinator::State> {
    let mut st = c.state.lock().unwrap();
    st.chinese_mode = true;
    st.input_buffer = "gong".to_string();
    st.candidates = texts.iter().map(|t| cand(t)).collect();
    st.selected_index = 0;
    st.current_page = 0;
    st.preedit = "gong".to_string();
    st
}

fn kept(st: &crate::coordinator::State) -> Vec<String> {
    st.candidates.iter().map(|c| c.text.clone()).collect()
}

/// `schema:<id>`：用码表方案的编码筛拼音候选；与文件来源混列时两边的码都生效。
#[test]
fn schema_source_filters_by_codetable_codes() {
    let id = wbx_id("schema");
    let files = format!(r#"["schema:{id}", "aux_code/flypy_test.txt"]"#);
    let (dir, _g) = data_dir_with_files("schema", &files, &id);
    let c = coord_with_data("schema_src", dir);
    c.engine_mgr.prewarm_text_codes(&id);
    let mut st = seed(&c, &["工", "攻", "公", "河"]);
    assert!(c.enter_aux_code(&mut st, keymap::VK_BACKTICK).is_some());
    let _ = c.handle_aux_code_key(&mut st, &key(vk_letter('A'), 0));
    assert_eq!(kept(&st), vec!["工", "攻"], "方案来源：a 命中工/攻");
    let _ = c.handle_aux_code_key(&mut st, &key(keymap::VK_BACK, 0));
    let _ = c.handle_aux_code_key(&mut st, &key(vk_letter('S'), 0));
    assert_eq!(kept(&st), vec!["河"], "文件来源的码同样生效");
}

/// 被引用方案里用户自己加的编码也能筛到（系统 + 用户词库是一个整体）。
#[test]
fn schema_source_includes_user_words() {
    let id = wbx_id("user");
    let files = format!(r#"["schema:{id}"]"#);
    let (dir, _g) = data_dir_with_files("user", &files, &id);
    let c = coord_with_data("schema_user", dir);
    c.store
        .as_ref()
        .unwrap()
        .add_user_word(&id, "zzzz", "嗨", 0, 0)
        .unwrap();
    c.engine_mgr.prewarm_text_codes(&id);
    let mut st = seed(&c, &["工", "嗨"]);
    assert!(c.enter_aux_code(&mut st, keymap::VK_BACKTICK).is_some());
    let _ = c.handle_aux_code_key(&mut st, &key(vk_letter('Z'), 0));
    assert_eq!(kept(&st), vec!["嗨"]);
}

/// 连续辅助码的首字母掩码（`AuxLookupNow::letter_mask`）把方案层用户词库的码算进去，
/// 与逐字母 `any_code_starts_with` 逐位相同。
#[test]
fn aux_lookup_now_mask_includes_user_layer() {
    use wind_engine::AuxCharLookup;
    let id = wbx_id("mask");
    let files = format!(r#"["schema:{id}", "aux_code/flypy_test.txt"]"#);
    let (dir, _g) = data_dir_with_files("mask", &files, &id);
    let c = coord_with_data("schema_mask", dir);
    c.store
        .as_ref()
        .unwrap()
        .add_user_word(&id, "zzzz", "嗨", 0, 0)
        .unwrap();
    c.store
        .as_ref()
        .unwrap()
        .add_user_word(&id, "xwyy", "工", 0, 0)
        .unwrap();
    c.engine_mgr.prewarm_text_codes(&id);
    let settings = c.engine_mgr.aux_code_settings();
    let rt = c.ensure_aux_code_runtime(&settings.sources);
    let lk = rt.lookup(&c.engine_mgr);
    assert!(lk.unready_schemas().is_empty(), "系统层已就绪");
    let bit = |b: u8| 1u32 << (b - b'a');
    assert_eq!(lk.letter_mask('嗨'), bit(b'z'), "只在用户层的字");
    assert_eq!(
        lk.letter_mask('工'),
        bit(b'a') | bit(b'x'),
        "系统层 + 用户层"
    );
    assert_eq!(lk.letter_mask('河'), bit(b's'), "文件层");
    for ch in ['嗨', '工', '攻', '公', '河', '李', '无'] {
        assert_eq!(
            lk.letter_mask(ch),
            crate::aux_code_source::aux_char_lookup_tests::default_mask(&lk, ch),
            "{ch}"
        );
    }
}

/// 系统层没就绪：不进入、不吞键——按键线程绝不现建反查索引。
#[test]
fn schema_source_not_ready_does_not_enter() {
    let id = wbx_id("cold");
    let files = format!(r#"["schema:{id}"]"#);
    let (dir, _g) = data_dir_with_files("cold", &files, &id);
    let c = coord_with_data("schema_cold", dir);
    let mut st = seed(&c, &["工", "攻"]);
    assert!(c.enter_aux_code(&mut st, keymap::VK_BACKTICK).is_none());
    assert_eq!(st.active, None);
    assert_eq!(kept(&st), vec!["工", "攻"], "候选原封不动");
    drop(st);
    // 门卫派出的后台构建建好后，下一次按键即可进入；等它落盘完，Cleanup 才清得干净。
    wait_index_ready(&c, &id);
}

/// 改了来源（override 层换 files）不切方案也要生效——此前缓存只在切方案时清。
#[test]
fn source_change_takes_effect_without_schema_switch() {
    let id = wbx_id("ovr");
    let (dir, _g) = data_dir_with_files("ovr", r#"["aux_code/flypy_test.txt"]"#, &id);
    let ov = dir.join("overrides");
    std::fs::create_dir_all(&ov).unwrap();
    let path = std::env::temp_dir().join(format!("wind_aux_code_ovr_{}.redb", std::process::id()));
    let _ = std::fs::remove_file(&path);
    let store = Arc::new(Store::open(&path).unwrap());
    let mut cfg = Config::default();
    cfg.schema.active = "pinyin".to_string();
    let c = Coordinator::new_headless_with_store_override(cfg, Some(&dir), store, Some(ov.clone()));
    let mut st = seed(&c, &["李", "樱", "河"]);
    assert!(c.enter_aux_code(&mut st, keymap::VK_BACKTICK).is_some());
    let _ = c.handle_aux_code_key(&mut st, &key(vk_letter('M'), 0));
    assert_eq!(kept(&st), vec!["李", "樱"]);
    let _ = c.handle_aux_code_key(&mut st, &key(keymap::VK_ESCAPE, 0));
    // 走设置页保存方案设置的真实入口：它负责让进入辅助码用的缓存失效。
    let ov_value: toml::Value = "[engine.aux_code]\nfiles = [\"aux_code/other.txt\"]\n"
        .parse()
        .unwrap();
    c.engine_mgr
        .write_schema_override("pinyin", &ov_value)
        .unwrap();
    assert!(c.enter_aux_code(&mut st, keymap::VK_BACKTICK).is_some());
    let _ = c.handle_aux_code_key(&mut st, &key(vk_letter('Q'), 0));
    assert_eq!(kept(&st), vec!["李", "樱", "河"], "新来源 other.txt 生效");
}

/// 轮询到该方案的反查索引建好（上限 5 秒）。
fn wait_index_ready(c: &Coordinator, id: &str) {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while c.engine_mgr.reverse_index_if_ready(id).is_none() {
        assert!(
            std::time::Instant::now() < deadline,
            "反查索引 5 秒内没有建好"
        );
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
}

/// 会话中途方案来源的反查索引被清掉（改方案设置 / 词库启用集变更都会整表清）：
/// 本次原样放行、后台重建，而不是只拿用户层去比、把候选滤到几乎全空。
#[test]
fn schema_layer_lost_mid_session_passes_through_and_rebuilds() {
    let id = wbx_id("lost");
    let files = format!(r#"["schema:{id}"]"#);
    let (dir, _g) = data_dir_with_files("lost", &files, &id);
    let c = coord_with_data("schema_lost", dir);
    c.engine_mgr.prewarm_text_codes(&id);
    let mut st = seed(&c, &["工", "攻", "公", "河"]);
    assert!(c.enter_aux_code(&mut st, keymap::VK_BACKTICK).is_some());
    let _ = c.handle_aux_code_key(&mut st, &key(vk_letter('A'), 0));
    assert_eq!(kept(&st), vec!["工", "攻"]);
    c.engine_mgr.invalidate_schema(&id);
    assert!(c.engine_mgr.reverse_index_if_ready(&id).is_none());
    let _ = c.handle_aux_code_key(&mut st, &key(vk_letter('T'), 0));
    assert_eq!(
        kept(&st),
        vec!["工", "攻", "公", "河"],
        "系统层没了：原样放行，不拿用户层单独筛"
    );
    drop(st);
    // 派出的后台重建真的把索引建回来了（也保证 Cleanup 之后不再有线程往缓存目录写）。
    wait_index_ready(&c, &id);
}

/// 启动预热覆盖辅助码引用的方案：否则每次启动后第一次按辅助码键都静默不进。
#[test]
fn prewarm_indexes_covers_aux_schema_sources() {
    let id = wbx_id("warm");
    let files = format!(r#"["schema:{id}"]"#);
    let (dir, _g) = data_dir_with_files("warm", &files, &id);
    let c = coord_with_data("schema_warm", dir);
    assert!(c.engine_mgr.reverse_index_if_ready(&id).is_none());
    c.prewarm_indexes();
    assert!(c.engine_mgr.reverse_index_if_ready(&id).is_some());
    let mut st = seed(&c, &["工", "攻"]);
    assert!(
        c.enter_aux_code(&mut st, keymap::VK_BACKTICK).is_some(),
        "预热后第一次按键即可进入"
    );
}

/// 相邻的文件来源合成一层（wind-aux-code 的「多表坍缩成单表」），只有方案来源各占一层；
/// 被方案来源隔开的文件段各自合并，清单顺序即优先级不变。
#[test]
fn adjacent_file_sources_collapse_into_one_layer() {
    let id = wbx_id("merge");
    let files = format!(
        r#"["aux_code/flypy_test.txt", "aux_code/other.txt", "schema:{id}", "aux_code/other.txt"]"#
    );
    let (dir, _g) = data_dir_with_files("merge", &files, &id);
    let c = coord_with_data("schema_merge", dir);
    let settings = c.engine_mgr.aux_code_settings();
    assert_eq!(settings.sources.len(), 4);
    let rt = c.ensure_aux_code_runtime(&settings.sources);
    assert_eq!(rt.layer_count(), 3, "文件段 + 方案 + 文件段");
    c.engine_mgr.prewarm_text_codes(&id);
    let mut st = seed(&c, &["李", "樱", "河", "工"]);
    assert!(c.enter_aux_code(&mut st, keymap::VK_BACKTICK).is_some());
    let _ = c.handle_aux_code_key(&mut st, &key(vk_letter('Q'), 0));
    assert_eq!(
        kept(&st),
        vec!["李", "樱", "河"],
        "合并层里低优表的码同样生效"
    );
}

/// 全拼默认辅助码引用笔画码表方案（`schema:stroke`）与直接读 `aux_code/stroke.txt`，筛选结果必须
/// 逐字一致：两份数据由 `gen_aux_code` 同一次解析产出，这里在 `hspnz` 全部 1~3 码上对拍。
///
/// 数据取 `build_dev/data`，缺则跳过（全仓惯例）；方案文件取入库的
/// `data/schemas/stroke.schema.toml`，id 换成本进程唯一的（见 [`write_wbx`]）。
#[test]
fn stroke_schema_source_filters_like_stroke_txt() {
    let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../..");
    let built = root.join("build_dev/data/schemas");
    let (txt, dict) = (
        built.join("aux_code/stroke.txt"),
        built.join("stroke/stroke.dict.yaml"),
    );
    if !txt.is_file() || !dict.is_file() {
        eprintln!(
            "跳过 stroke_schema_source_filters_like_stroke_txt：缺 {} 或 {}（先跑 gen-data）",
            txt.display(),
            dict.display()
        );
        return;
    }
    let schema_text = std::fs::read_to_string(root.join("data/schemas/stroke.schema.toml"))
        .expect("入库的 stroke.schema.toml");
    let id = wbx_id("stroke");
    let (dir, _g) = data_dir_with_files("stroke", r#"["aux_code/stroke.txt"]"#, &id);
    let schemas = dir.join("schemas");
    std::fs::copy(&txt, schemas.join("aux_code/stroke.txt")).unwrap();
    std::fs::create_dir_all(schemas.join("stroke")).unwrap();
    std::fs::copy(&dict, schemas.join("stroke/stroke.dict.yaml")).unwrap();
    let replaced = schema_text.replace("id = \"stroke\"", &format!("id = \"{id}\""));
    assert_ne!(replaced, schema_text, "方案文件里应有 id = \"stroke\"");
    std::fs::write(schemas.join(format!("{id}.schema.toml")), replaced).unwrap();
    // 第二份 data_dir：同一套文件，只把 pinyin 的来源换成方案引用。
    let dir_schema = dir.with_file_name(format!(
        "{}_schema",
        dir.file_name().unwrap().to_string_lossy()
    ));
    let _ = std::fs::remove_dir_all(&dir_schema);
    copy_dir(&dir, &dir_schema);
    let _g2 = Cleanup {
        id: id.clone(),
        dir: dir_schema.clone(),
    };
    let pinyin = dir_schema.join("schemas/pinyin.schema.toml");
    let text = std::fs::read_to_string(&pinyin).unwrap();
    let swapped = text.replace(r#"["aux_code/stroke.txt"]"#, &format!(r#"["schema:{id}"]"#));
    assert_ne!(swapped, text, "pinyin 的来源应已换成方案引用");
    std::fs::write(&pinyin, swapped).unwrap();

    let by_file = coord_with_data("stroke_file", dir);
    let by_schema = coord_with_data("stroke_schema", dir_schema);
    by_schema.engine_mgr.prewarm_text_codes(&id);

    // 「li」一页的常见候选 + 几个词组（词组走逐字首码）+ 一个无码字符。
    let texts = [
        "李", "里", "理", "力", "利", "立", "离", "例", "历", "丽", "礼", "黎", "粒", "莉", "梨",
        "璃", "哩", "栗", "历史", "利用", "理解", "力量", "里面", "A",
    ];
    let letters = ['h', 's', 'p', 'n', 'z'];
    let mut inputs: Vec<String> = Vec::new();
    for a in letters {
        for b in letters {
            for c in letters {
                inputs.push([a, b, c].iter().collect());
            }
        }
    }
    let mut narrowed = 0usize;
    for input in &inputs {
        let run = |c: &Arc<Coordinator>| -> Vec<Vec<String>> {
            let mut st = seed(c, &texts);
            assert!(c.enter_aux_code(&mut st, keymap::VK_BACKTICK).is_some());
            let steps = input
                .chars()
                .map(|ch| {
                    let vk = vk_letter(ch.to_ascii_uppercase());
                    let _ = c.handle_aux_code_key(&mut st, &key(vk, 0));
                    kept(&st)
                })
                .collect();
            let _ = c.handle_aux_code_key(&mut st, &key(keymap::VK_ESCAPE, 0));
            steps
        };
        let (a, b) = (run(&by_file), run(&by_schema));
        assert_eq!(a, b, "辅助码 {input}：文件来源与方案来源筛出的候选不同");
        narrowed += a
            .iter()
            .filter(|k| !k.is_empty() && k.len() < texts.len())
            .count();
    }
    // 反向保证：确实在筛（不是两边都原样放行而恒等）。
    assert!(
        narrowed > 100,
        "有效筛选步数过少（{narrowed}），对拍多半没测到东西"
    );
    // 抽查一条已知码：「力」的上游笔画码是 zp（折撇）。
    let mut st = seed(&by_schema, &texts);
    assert!(
        by_schema
            .enter_aux_code(&mut st, keymap::VK_BACKTICK)
            .is_some()
    );
    let _ = by_schema.handle_aux_code_key(&mut st, &key(vk_letter('Z'), 0));
    let _ = by_schema.handle_aux_code_key(&mut st, &key(vk_letter('P'), 0));
    assert!(kept(&st).contains(&"力".to_string()), "{:?}", kept(&st));
}

/// 递归复制目录（测试夹具用）。
fn copy_dir(from: &std::path::Path, to: &std::path::Path) {
    std::fs::create_dir_all(to).unwrap();
    for e in std::fs::read_dir(from).unwrap().flatten() {
        let dst = to.join(e.file_name());
        if e.file_type().unwrap().is_dir() {
            copy_dir(&e.path(), &dst);
        } else {
            std::fs::copy(e.path(), dst).unwrap();
        }
    }
}
