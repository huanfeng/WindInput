//! 会话态动词 `commit_highlighted`（上屏当前高亮候选）的端到端测试。
//!
//! 它就是「空格上屏高亮候选」那个动作换一个键来按，所以断言的形状是**对拍**：同一段输入、
//! 同一个高亮位置，一边按空格、一边按绑了本动词的 Tab，两边的按键结果（含补空格）、记账
//! 事件（统计来源）、收尾模式与上屏历史必须逐字相同。只断言「上屏了高亮那条」是不够的——
//! 这些副作用各模式出口各有一套，另写一份的实现也能让文本对上，却在副作用上悄悄分叉。
//!
//! 词频**不在断言之列**：headless 构造不带用户库（`store = None`），选词记词频是空操作，
//! 两边比出来恒等、证明不了什么。词频与其余副作用同在各模式的选中出口里，对拍住出口
//! （按键结果 + 统计 + 历史）即覆盖到它所在的那个函数。
//!
//! 每个模式都先断言**确实进了该模式**：触发键没生效时按键会落回主输入路径，而主路径同样会
//! 上屏——不验进入就是假绿（见 docs/design/session-key-actions.md §10）。
//!
//! ⚠️ 依赖 `build_dev/data` 真实词库；缺失时**静默跳过**（判据是耗时 0.00s）。

use std::path::{Path, PathBuf};
use std::sync::Arc;
use wind_bridge::handler::{KeyAction, KeyEventData, MessageHandler};
use wind_config::Config;
use wind_coordinator::Coordinator;
use wind_ipc::protocol::{EVENT_KEY_DOWN, MOD_SHIFT};

const VK_TAB: u32 = 0x09;
const VK_SPACE: u32 = 0x20;
const VK_DOWN: u32 = 0x28;
const VK_SEMICOLON: u32 = 0xBA;
const VK_BACKTICK: u32 = 0xC0;

fn data_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../build_dev/data")
}

fn has_data() -> bool {
    let d = data_dir();
    ["wubi86", "pinyin", "english"]
        .iter()
        .all(|s| d.join(format!("schemas/{s}.schema.toml")).exists())
}

macro_rules! skip_without_data {
    () => {
        if !has_data() {
            eprintln!("跳过：缺 build_dev 词库");
            return;
        }
    };
}

fn key(k: u32, modifiers: u32) -> KeyEventData {
    KeyEventData {
        key_code: k,
        scan_code: 0,
        modifiers,
        event_type: EVENT_KEY_DOWN,
        toggles: 0,
        event_seq: 0,
        prev_char: 0,
        seq_char: 0,
    }
}

fn open_with(edit: impl FnOnce(&mut Config)) -> Arc<Coordinator> {
    open_full(edit, None)
}

/// `ov`：方案 override 目录（特殊模式要给码表方案挂 `[overlay]` 段）。
fn open_full(edit: impl FnOnce(&mut Config), ov: Option<&Path>) -> Arc<Coordinator> {
    let mut c = Config::default();
    c.schema.available = vec!["wubi86".into(), "pinyin".into(), "english".into()];
    c.schema.active = "wubi86".into();
    c.input.default.chinese_mode = true;
    c.input.temp_english.enabled = true;
    edit(&mut c);
    Coordinator::new_headless_with_override(c, Some(&data_dir()), ov.map(Path::to_path_buf))
}

/// 绑了 `tab = "commit_highlighted"` 的实例。显式写进 `session_actions` 的键压过出厂
/// `highlight_keys` 折算出的 `tab = highlight_down`。
fn bind_tab(c: &mut Config) {
    c.keys
        .session_actions
        .insert("tab".into(), "commit_highlighted".into());
}

fn type_str(coord: &Coordinator, s: &str) {
    for ch in s.chars() {
        coord.handle_key_event(&key((ch.to_ascii_uppercase() as u32) & 0xFF, 0));
    }
}

/// 一次按键的可比对快照：按键结果、这次按键产生的记账事件、按完后的模式。
///
/// `KeyAction` 没有 `PartialEq`，按 `Debug` 形态比——字段全在里面，比结构更严。
fn press_and_observe(coord: &Coordinator, vk: u32) -> (String, String, Option<&'static str>) {
    coord.debug_capture_stat_events();
    let act = coord.handle_key_event(&key(vk, 0));
    let stats = format!("{:?}", coord.debug_take_stat_events());
    (format!("{act:?}"), stats, coord.debug_active_mode())
}

/// ★ 对拍主体：两个实例走同一段 `setup`，一个按空格、一个按 Tab，结果必须逐字相同。
///
/// `move_highlight`：先按 ↓ 把高亮移到第 2 条再比。**高亮不在首位时才分得出**「上屏高亮」
/// 与「上屏首选」——在首位上比，一个上屏首选的错误实现也会绿。
///
/// 两边都先摆好再按键：两个实例若共用任何学习态，先上屏的一方会改掉另一方的候选序。
fn assert_same_as_space(
    mode: Option<&str>,
    edit: impl Fn(&mut Config),
    edit_tab: impl Fn(&mut Config),
    setup: impl Fn(&Coordinator),
    move_highlight: bool,
) {
    assert_same_as_space_in(None, mode, edit, edit_tab, setup, move_highlight);
}

fn assert_same_as_space_in(
    ov: Option<&Path>,
    mode: Option<&str>,
    edit: impl Fn(&mut Config),
    edit_tab: impl Fn(&mut Config),
    setup: impl Fn(&Coordinator),
    move_highlight: bool,
) {
    let space = open_full(&edit, ov);
    let tab = open_full(
        |c| {
            edit(c);
            edit_tab(c);
            bind_tab(c);
        },
        ov,
    );
    for coord in [&space, &tab] {
        setup(coord);
        assert_eq!(coord.debug_active_mode(), mode, "前提：应处在 {mode:?}");
        let texts = coord.debug_page_texts();
        assert!(!texts.is_empty(), "前提：应有候选");
        if move_highlight {
            assert!(
                texts.len() > 1,
                "前提：至少两条候选才能移高亮，实际 {texts:?}"
            );
            coord.handle_key_event(&key(VK_DOWN, 0));
            let (_, sel, _) = coord.debug_page_info();
            assert_eq!(sel, 1, "前提：↓ 应把高亮移到第 2 条");
        }
    }
    let highlighted = {
        let (_, sel, _) = tab.debug_page_info();
        tab.debug_page_texts()[sel].clone()
    };

    let by_space = press_and_observe(&space, VK_SPACE);
    let by_tab = press_and_observe(&tab, VK_TAB);

    assert!(
        by_space.0.contains("InsertText"),
        "前提：空格应上屏，实际 {}",
        by_space.0
    );
    assert!(
        by_space.0.contains(&format!("{highlighted:?}"))
            || by_space.0.contains(&highlighted.to_string()),
        "前提：空格上屏的应是高亮候选 {highlighted:?}，实际 {}",
        by_space.0
    );
    assert_eq!(by_tab.0, by_space.0, "按键结果应与空格逐字相同");
    assert_eq!(by_tab.1, by_space.1, "记账事件应与空格逐字相同");
    assert_eq!(by_tab.2, by_space.2, "按完后的模式应与空格相同");
    assert_same_history(&space, &tab);
}

/// 上屏历史对拍：两边都回到普通输入后，`;` 进快捷输入，首条就是「重复上屏」候选——
/// 它取自上屏历史的最近一条（`quick_input.repeat`）。
fn assert_same_history(space: &Coordinator, tab: &Coordinator) {
    if space.debug_active_mode().is_some() {
        return; // 还留在模式里（部分上屏）：历史对拍无从谈起，按键结果已比过
    }
    let repeat_head = |c: &Coordinator| {
        c.handle_key_event(&key(VK_SEMICOLON, 0));
        assert_eq!(c.debug_active_mode(), Some("mix"), "前提：`;` 进快捷输入");
        c.debug_page_texts().first().cloned()
    };
    let (a, b) = (repeat_head(space), repeat_head(tab));
    assert!(a.is_some(), "前提：上屏后应有可重复上屏的历史");
    assert_eq!(b, a, "上屏历史应与空格相同");
}

#[test]
fn main_route_commits_highlighted_like_space() {
    skip_without_data!();
    assert_same_as_space(None, |_| {}, |_| {}, |c| type_str(c, "a"), true);
}

/// ⚠️ 本条**不区分「绑上了」与「没绑上」**：临拼的兜底臂对任何未认领的键都「上屏高亮」
/// （`handle_temp_pinyin_key` 的 `_` 臂），Tab 没绑上时照样得到同一结果。它守的是对拍——
/// 出口日后若分叉，这里会红；「动词真的生效」由其余模式的用例证明。
#[test]
fn temp_pinyin_commits_highlighted_like_space() {
    skip_without_data!();
    assert_same_as_space(
        Some("temp_pinyin"),
        |_| {},
        |_| {},
        |c| {
            c.handle_key_event(&key(VK_BACKTICK, 0));
            type_str(c, "ni");
        },
        true,
    );
}

fn enter_temp_english(c: &Coordinator) {
    c.handle_key_event(&key(u32::from(b'H'), MOD_SHIFT));
    type_str(c, "el");
}

#[test]
fn temp_english_commits_highlighted_like_space() {
    skip_without_data!();
    assert_same_as_space(
        Some("temp_english"),
        |_| {},
        |_| {},
        enter_temp_english,
        true,
    );
}

/// ★ 临英 `space_as_input` 下空格是输入字符，**本动词不跟这个变体**：它恒为「上屏高亮」。
///
/// 对照组是**关着**该开关按空格——那才是「上屏高亮」这个动作本身。
#[test]
fn temp_english_space_as_input_does_not_bend_the_verb() {
    skip_without_data!();
    assert_same_as_space(
        Some("temp_english"),
        |_| {},
        |c| c.input.temp_english.space_as_input = true,
        enter_temp_english,
        true,
    );
}

#[test]
fn quick_input_commits_highlighted_like_space() {
    skip_without_data!();
    assert_same_as_space(
        Some("mix"),
        |_| {},
        |_| {},
        |c| {
            c.handle_key_event(&key(VK_SEMICOLON, 0));
            type_str(c, "ni");
        },
        true,
    );
}

/// 生僻字模式与特殊模式共用 `handle_special_key` 与同一个选中出口。
#[test]
fn rare_char_mode_commits_highlighted_like_space() {
    skip_without_data!();
    let bind = |c: &mut Config| {
        c.keys
            .key_actions
            .insert("backslash".into(), "rare_char".into());
    };
    assert_same_as_space(
        Some("rare_char"),
        bind,
        |_| {},
        |c| {
            c.handle_key_event(&key(0xDC, 0));
            type_str(c, "a");
        },
        true,
    );
}

/// 非生僻的特殊模式（`special:<id>`）：主方案拼音，`\` 进以 wubi86 为码表的特殊模式
/// （经 override 给它挂 `[overlay]` 段）。与生僻字模式同一个 `handle_special_key`，但
/// `ModeKind::Special(_)` 是另一个变体，派发臂要各自覆盖。
#[test]
fn special_mode_commits_highlighted_like_space() {
    skip_without_data!();
    let ov = std::env::temp_dir().join(format!("wind_kv_special_ov_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&ov);
    std::fs::create_dir_all(&ov).unwrap();
    std::fs::write(ov.join("wubi86.toml"), "[overlay]\nkind = \"special\"\n").unwrap();
    let edit = |c: &mut Config| {
        c.schema.active = "pinyin".into();
        c.keys
            .key_actions
            .insert("backslash".into(), "special:wubi86".into());
    };
    assert_same_as_space_in(
        Some(&ov),
        Some("special"),
        edit,
        |_| {},
        |c| {
            c.handle_key_event(&key(0xDC, 0));
            type_str(c, "a");
        },
        true,
    );
    let _ = std::fs::remove_dir_all(&ov);
}

/// 辅助码：拼音打 `shi`、`\` 进辅助码（全局开关须拨开）、筛一码 `h`（笔画「横」）后移高亮。
/// 走的是 `aux_code_committed`——部分消费时要留在模式内，派发不能套主路的 `commit_selected`。
///
/// ⚠️ 必须先 `prewarm_indexes`：全拼默认辅助码 2a9193ba 起从文件（`aux_code/stroke.txt`，
/// 进程内同步读，无需反查索引）改成引用码表方案（`schema:stroke`），`enter_aux_code` 门卫
/// 要求该方案反查索引已就绪、按键线程绝不现建（见 `coordinator.rs::prewarm_indexes` 的注释）。
/// 真机由 `Coordinator::new` 起的后台线程预热，headless 不跑那个线程，不手动补这一句，
/// 本用例会在索引未就绪的那次按键上静默不进模式（`enter_aux_code` 直接返回 `None`）。
#[test]
fn aux_code_commits_highlighted_like_space() {
    skip_without_data!();
    let edit = |c: &mut Config| {
        c.schema.active = "pinyin".into();
        c.schema.pinyin.aux_code.enabled = true;
        c.keys
            .key_actions
            .insert("backslash".into(), "aux_code".into());
    };
    assert_same_as_space(
        Some("aux_code"),
        edit,
        |_| {},
        |c| {
            c.prewarm_indexes();
            type_str(c, "shi");
            c.handle_key_event(&key(0xDC, 0));
            type_str(c, "h");
        },
        true,
    );
}

/// mix 重复上屏特判：空缓冲进快捷输入时首条是「重复上屏」候选，空格整体上屏它、不记选词
/// （`commit_mix_repeat`）。派发里这一格是守卫臂，漏了就会落到 `mix_select` 写孤儿词频行。
///
/// 不移高亮：重复上屏态下空格恒取首条（不看高亮），移了反而比不出「高亮那条」。
#[test]
fn quick_input_repeat_commits_like_space() {
    skip_without_data!();
    assert_same_as_space(
        Some("mix"),
        |_| {},
        |_| {},
        |c| {
            type_str(c, "a");
            c.handle_key_event(&key(VK_SPACE, 0));
            c.handle_key_event(&key(VK_SEMICOLON, 0));
        },
        false,
    );
}

/// ★ 重复上屏态下候选**恒只有一条**——`commit_highlighted` 的 mix 臂不为重复态另开
/// 分支（交给 `mix_select_at` 的 `mix_repeat` 判断）所依赖的前提。
///
/// 用最可能破坏它的情形压：简入繁出开着、上次上屏是个 1 对多的字（「出」→ 出 / 齣）。
/// 普通候选区里这种字会被 `expand_s2t_variants` 就地展开成多条；若重复态也被展开，高亮就能
/// 移到第 2 条，空格取首条还是取高亮就成了两个结果，那个被删掉的分支就不再冗余。
#[test]
fn quick_input_repeat_holds_a_single_candidate_even_with_s2t_variants() {
    skip_without_data!();
    let coord = open_with(|c| c.schema.active = "pinyin".into());
    if !coord.debug_set_s2t(true) {
        eprintln!("跳过：缺少 opencc 数据");
        return;
    }
    type_str(&coord, "chu");
    let texts = coord.debug_page_texts();
    let p = texts
        .iter()
        .position(|t| t == "出")
        .expect("前提：chu 的候选里应有「出」");
    assert!(
        coord.debug_page_texts().len() > p + 1 && coord.debug_page_texts()[p + 1] == "出",
        "前提：普通候选区里「出」会被展开出变体（内部 text 同为「出」），实际 {texts:?}"
    );
    coord.handle_key_event(&key(0x31 + p as u32, 0)); // 选「出」上屏，进上屏历史

    coord.handle_key_event(&key(VK_SEMICOLON, 0));
    assert_eq!(
        coord.debug_active_mode(),
        Some("mix"),
        "前提：`;` 进快捷输入"
    );
    assert_eq!(
        coord.debug_page_texts(),
        vec!["出".to_string()],
        "重复上屏态下只应有一条候选（不做变体展开）"
    );
    coord.handle_key_event(&key(VK_DOWN, 0));
    let (_, sel, _) = coord.debug_page_info();
    assert_eq!(sel, 0, "只有一条候选，高亮移不走");
}

// ───────────────────────── 联想态：跟随 space_commits ─────────────────────────

fn assoc_dict_ready() -> bool {
    data_dir()
        .join("schemas/wubi86/wubi86_jidian.dict.yaml")
        .exists()
}

/// `tab`：Tab 的会话态绑定；`None` = 不动出厂（`highlight_down`）。
fn open_assoc(space_commits: bool, tab: Option<&str>) -> Arc<Coordinator> {
    let mut c = Config::default();
    c.schema.available = vec!["wubi86_pinyin".into()];
    c.schema.active = "wubi86_pinyin".into();
    c.input.default.chinese_mode = true;
    c.input.symbol.smart_mode = false;
    c.input.association.kind = "word".into();
    c.input.association.space_commits = space_commits;
    if let Some(verb) = tab {
        c.keys.session_actions.insert("tab".into(), verb.into());
    }
    let coord = Coordinator::new_headless(c, Some(&data_dir()));
    coord.prewarm_indexes();
    coord
}

/// 打 `q` 空格上屏「我」，进联想态；返回联想候选。
fn enter_assoc(c: &Coordinator) -> Vec<String> {
    type_str(c, "q");
    c.handle_key_event(&key(VK_SPACE, 0));
    let hits = c.debug_assoc_texts();
    assert!(!hits.is_empty(), "前提：应进联想态");
    hits
}

/// `space_commits = true`（默认）：联想态按绑定键与按空格一样，选中高亮联想。
#[test]
fn assoc_space_commits_true_follows_space() {
    skip_without_data!();
    if !assoc_dict_ready() {
        return;
    }
    let space = open_assoc(true, None);
    let tab = open_assoc(true, Some("commit_highlighted"));
    enter_assoc(&space);
    enter_assoc(&tab);
    let by_space = press_and_observe(&space, VK_SPACE);
    let by_tab = press_and_observe(&tab, VK_TAB);
    assert!(
        by_space.0.contains("InsertText") || by_space.0.contains("CommitThenDefer"),
        "前提：空格应选中联想，实际 {}",
        by_space.0
    );
    assert_eq!(by_tab.0, by_space.0, "按键结果应与空格逐字相同");
    assert_eq!(by_tab.1, by_space.1, "记账事件应与空格逐字相同");
}

/// ★ `space_commits = false`：联想的高亮是输入法猜的，用户说了「别替我选」——绑定键
/// 同样**不选**，也不吞键（返回原语义，与按一个没绑定的键同形）。
///
/// 对照组是同配置下**没绑** Tab 的实例：两边按 Tab 的结果必须相同。只断言「没上屏联想」
/// 的话，一个把键吞掉什么都不做的实现也会绿。
#[test]
fn assoc_space_commits_false_is_not_committed_by_verb() {
    skip_without_data!();
    if !assoc_dict_ready() {
        return;
    }
    let bound = open_assoc(false, Some("commit_highlighted"));
    // 对照组：Tab 出厂是 `highlight_down`，也会在联想态被吃去移高亮；显式写 `none`
    // 才是「这个键没有会话态绑定」的原语义。
    let unbound = open_assoc(false, Some("none"));
    let hits = enter_assoc(&bound);
    enter_assoc(&unbound);

    let act = bound.handle_key_event(&key(VK_TAB, 0));
    let dbg = format!("{act:?}");
    for h in &hits {
        assert!(!dbg.contains(h.as_str()), "不该选中联想 {h:?}，实得 {dbg}");
    }
    assert!(
        !matches!(act, KeyAction::Consumed),
        "不选就不该吞键，实得 {dbg}"
    );
    let reference = unbound.handle_key_event(&key(VK_TAB, 0));
    assert_eq!(
        dbg,
        format!("{reference:?}"),
        "应与该键没有绑定时的原语义相同"
    );
}

/// 无会话：Tab 归宿主（制表符），不被本动词截走。
#[test]
fn idle_tab_is_not_taken() {
    skip_without_data!();
    let coord = open_with(bind_tab);
    let act = coord.handle_key_event(&key(VK_TAB, 0));
    assert!(
        matches!(act, KeyAction::PassThrough),
        "空闲时 Tab 应透传，实际 {act:?}"
    );
}

/// ★ 有会话**无候选**：本动词无事可做，键回落原语义，不得清掉或上屏正在打的内容。
///
/// 网址模式是「有会话、从不产候选」的天然样本（出厂开关关闭，须显式拨开）。
#[test]
fn session_without_candidates_falls_back() {
    skip_without_data!();
    let coord = open_with(|c| {
        bind_tab(c);
        c.input.url.enabled = true;
    });
    type_str(&coord, "www");
    coord.handle_key_event(&key(0xBE, 0)); // .
    assert_eq!(coord.debug_active_mode(), Some("url"), "前提：应进网址模式");
    assert!(coord.debug_page_texts().is_empty(), "前提：网址模式无候选");

    let act = coord.handle_key_event(&key(VK_TAB, 0));
    assert!(
        !matches!(act, KeyAction::InsertText { .. }),
        "无候选时不得上屏，实际 {act:?}"
    );
    assert_eq!(coord.debug_active_mode(), Some("url"), "不得退出网址模式");
}
