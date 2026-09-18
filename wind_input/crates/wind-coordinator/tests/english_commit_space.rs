//! 英文方案「上屏后自动补空格」（`schema.english.commit_space`）端到端测试。
//!
//! 该配置项曾经**四层就位、零处生效**：结构体 / schema 注册 / config.toml / 设置页 GUI
//! 全有，消费点却只接在 `commit_candidate` 上——而英文引擎恒 `should_commit = false`、
//! 自动上屏又只认码表来源，那三个调用点在英文方案下**一个都走不到**。开关打开毫无反应。
//!
//! 所以本文件的断言重心不是「补了空格」，而是**逐条通路各自补没补**：真正的英文上屏出口
//! 是 `commit_selected`（六类触发汇于一处）与「空格上屏原码」，而回车上屏原码、标点顶屏
//! 刻意不补。少测一条，下一次重构就可能把某条通路悄悄改回不生效。
//!
//! ⚠️ 词典缺失时整族静默跳过（判据是**耗时 0.00s**，不是通过条数）——见
//! `build_dev/data` 相关记录。worktree 中需自备 `build_dev` 链接。

use std::path::PathBuf;
use std::sync::Arc;
use wind_bridge::handler::{CommitRequestData, KeyAction, KeyEventData, MessageHandler};
use wind_config::Config;
use wind_config::config::RawCandidateMode;
use wind_coordinator::Coordinator;
use wind_ipc::protocol::{EVENT_KEY_DOWN, EVENT_KEY_UP, MOD_SHIFT};

const VK_SPACE: u32 = 0x20;
const VK_RETURN: u32 = 0x0D;
const VK_2: u32 = 0x32;
const VK_3: u32 = 0x33;

fn data_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../build_dev/data")
}

/// 英文方案就绪判据：方案定义与**词库**都要在（词库由构建 assemble 注入，不进 git）。
fn has_english_schema() -> bool {
    let d = data_dir();
    d.join("schemas/english.schema.toml").exists() && d.join("schemas/english").is_dir()
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

fn english_config(commit_space: bool) -> Config {
    let mut cfg = Config::default();
    cfg.schema.available = vec!["english".into(), "wubi86".into()];
    cfg.schema.active = "english".into();
    cfg.input.default.chinese_mode = true;
    cfg.schema.english.commit_space = commit_space;
    cfg
}

fn press_letter(coord: &Coordinator, c: char) -> KeyAction {
    let vk = (c.to_ascii_uppercase() as u32) & 0xFF;
    coord.handle_key_event(&key_event(vk, EVENT_KEY_DOWN))
}

/// Shift+字母：英文方案下走主路字母臂，大写落进影子串 `input_buffer_cased`，
/// `input_buffer` 保持小写（见 `handle_lifecycle` 临英门禁处的 ★ 英文方案下不进临英）。
fn press_letter_shift(coord: &Coordinator, c: char) -> KeyAction {
    let vk = (c.to_ascii_uppercase() as u32) & 0xFF;
    let mut ev = key_event(vk, EVENT_KEY_DOWN);
    ev.modifiers = MOD_SHIFT;
    coord.handle_key_event(&ev)
}

fn type_word(coord: &Coordinator, s: &str) {
    for c in s.chars() {
        press_letter(coord, c);
    }
}

fn commit_text(action: &KeyAction) -> String {
    match action {
        KeyAction::InsertText { text, .. } => text.clone(),
        other => panic!("应为 InsertText 上屏，实际: {other:?}"),
    }
}

/// 空格选中首选 → 上屏「首选 + 空格」。
///
/// 断言的是**相对关系**（首选文本 + 一个空格）而非硬编码 `"hello "`：词库内容与排序会随
/// 版本变，绑死具体单词只会让测试在无关改动上误报。
#[test]
fn space_select_appends_space() {
    if !has_english_schema() {
        eprintln!("跳过：缺少英文方案或词库");
        return;
    }
    let coord = Coordinator::new_headless(english_config(true), Some(&data_dir()));
    assert_eq!(coord.active_schema_id(), "english");

    type_word(&coord, "hel");
    let top = coord.debug_page_texts().first().cloned().expect("应有候选");

    let text = commit_text(&coord.handle_key_event(&key_event(VK_SPACE, EVENT_KEY_DOWN)));
    assert_eq!(text, format!("{top} "), "空格选首选后应补一个空格");
}

/// 首字母大写时**选中首选**上屏，同样要补空格（t122）。
///
/// 与 `space_select_appends_space` 只差一个 Shift，断言同形。**这条才是 t122 的复现。**
///
/// 它落在 `english_appends_space` 上（有候选 → 选中上屏）。此刻的首选是
/// `english_head_candidates` 用「所打原码」直接构造的原文候选（`source` 为 `None`，
/// 词库里字面相同的那条已在上游被精确去重吃掉），于是第一分支不命中；第二分支比
/// `text == input`，`input` 若传成全小写的 `state.input_buffer` 就永不相等 —— 两头落空。
///
/// 触发开关是 `raw_candidate`（默认开）而**不是** `case_follow_input`：
/// `raw_candidate = false` 时首选是词库候选、`source == English`，第一分支直接命中。
///
/// 反向验证（2026-09-14 实跑）：把调用点的 `cased_or_buffer(...)` 换回 `&state.input_buffer`，
/// 本测试即变红（其余 12 条不动）。
#[test]
fn space_select_appends_space_when_capitalized() {
    if !has_english_schema() {
        return;
    }
    let coord = Coordinator::new_headless(english_config(true), Some(&data_dir()));

    press_letter_shift(&coord, 'h');
    type_word(&coord, "el");
    let top = coord.debug_page_texts().first().cloned().expect("应有候选");
    assert!(
        top.starts_with('H'),
        "前提：首选应已按输入投影成首字母大写，实际: {top}"
    );

    let text = commit_text(&coord.handle_key_event(&key_event(VK_SPACE, EVENT_KEY_DOWN)));
    assert_eq!(text, format!("{top} "), "首字母大写选首选后应补一个空格");
}

/// 反向对照：开关关闭时**不得**补空格。
///
/// 没有这一条，「恒补空格」的实现也能让上面那条通过——本项的缺陷史正是「判据看着对、
/// 实际恒不生效」，反向对照是唯一能同时排除「恒开」与「恒关」的断言。
#[test]
fn space_select_no_space_when_disabled() {
    if !has_english_schema() {
        return;
    }
    let coord = Coordinator::new_headless(english_config(false), Some(&data_dir()));

    type_word(&coord, "hel");
    let top = coord.debug_page_texts().first().cloned().expect("应有候选");

    let text = commit_text(&coord.handle_key_event(&key_event(VK_SPACE, EVENT_KEY_DOWN)));
    assert_eq!(text, top, "开关关闭时不得补空格");
}

/// 数字键选词同样补空格——锁住「所有选中方式一律补」的拍板结论。
///
/// 空格与数字键在 `commit_selected` 内部汇于同一分支，但那是**当前**的实现事实；本条
/// 存在的意义是：日后若有人按触发键分流，这条会立刻失败而不是静默漂移。
#[test]
fn number_key_select_appends_space() {
    if !has_english_schema() {
        return;
    }
    let coord = Coordinator::new_headless(english_config(true), Some(&data_dir()));

    type_word(&coord, "hel");
    let page = coord.debug_page_texts();
    assert!(
        page.len() >= 2,
        "需至少两个候选才能测数字键 2，实际 {page:?}"
    );
    let second = page[1].clone();

    let text = commit_text(&coord.handle_key_event(&key_event(VK_2, EVENT_KEY_DOWN)));
    assert_eq!(text, format!("{second} "), "数字键选词后应补一个空格");
}

/// 鼠标点选同样补空格（第三类触发，走 `mouse_select` → `commit_selected` 主输入路）。
#[test]
fn mouse_select_appends_space() {
    if !has_english_schema() {
        return;
    }
    let coord = Coordinator::new_headless(english_config(true), Some(&data_dir()));

    type_word(&coord, "hel");
    let top = coord.debug_page_texts().first().cloned().expect("应有候选");

    let act = coord
        .debug_mouse_select(0)
        .expect("鼠标点选应返回主输入路 action");
    assert_eq!(
        commit_text(&act),
        format!("{top} "),
        "鼠标点选后应补一个空格"
    );
}

/// 打词库里没有的词，**空格**上屏原码 → 也补空格（用户拍板：与选中候选一致）。
///
/// 这条走的是与选词完全不同的出口（`VK_SPACE` 空码分支），判据也不同——那里没有候选可依，
/// 用的是方案口径 `english_space_enabled`。
///
/// ⚠️ **必须显式关掉 `raw_candidate`**：它默认开，会把所打原文作为首候选钉进列表，于是
/// 「无候选」这个前提不再成立、本条要测的兜底出口根本走不到（走的是选中首候选那条）。
/// 有原文候选时的对应行为另见 `tests/english_head_candidates.rs`。
#[test]
fn raw_code_space_commit_appends_space() {
    if !has_english_schema() {
        return;
    }
    let mut cfg = english_config(true);
    cfg.schema.english.raw_candidate = RawCandidateMode::Off;
    let coord = Coordinator::new_headless(cfg, Some(&data_dir()));

    // 前提：该串在词库中无候选。若日后词库收录了它，前提失效 → 显式失败而非静默变成
    // 「测了另一条通路」。
    let nonsense = "qwxzjv";
    type_word(&coord, nonsense);
    assert_eq!(
        coord.debug_candidate_count(),
        0,
        "测试前提失效：{nonsense} 现在有候选了，请换一个无候选的串"
    );

    let text = commit_text(&coord.handle_key_event(&key_event(VK_SPACE, EVENT_KEY_DOWN)));
    assert_eq!(text, format!("{nonsense} "), "空格上屏原码后应补一个空格");
}

/// 首字母大写时，「空格上屏原码」的**空码兜底分支**照样补空格。
///
/// ⚠️ **这条不是 t122 的复现**（复现见 `space_select_appends_space_when_capitalized`）。
/// 它走的是 `message_handler` 里那条无候选兜底路径，判据只有一句 `english_space_enabled_in`
/// ——不过候选级判据，所以缺陷期它就是绿的。留着是为了钉住「这条通路对大小写不敏感」，
/// 免得日后有人把候选级判据一并塞进这里。
///
/// 我最初误把它当成 t122 的复现，它一绿就差点据此判定「根因不是大写」。记在这里。
#[test]
fn raw_code_space_commit_appends_space_when_capitalized() {
    if !has_english_schema() {
        return;
    }
    let mut cfg = english_config(true);
    cfg.schema.english.raw_candidate = RawCandidateMode::Off;
    let coord = Coordinator::new_headless(cfg, Some(&data_dir()));

    // 与小写那条同一个无候选串，只把首字母改成大写输入。
    let nonsense = "qwxzjv";
    press_letter_shift(&coord, 'q');
    type_word(&coord, &nonsense[1..]);
    assert_eq!(
        coord.debug_candidate_count(),
        0,
        "测试前提失效：{nonsense} 现在有候选了，请换一个无候选的串"
    );

    let text = commit_text(&coord.handle_key_event(&key_event(VK_SPACE, EVENT_KEY_DOWN)));
    assert_eq!(text, "Qwxzjv ", "首字母大写的原码上屏后应补一个空格");
}

/// 回车上屏原码 **不补**空格——刻意的不对称，不是漏接。
///
/// 回车分支与空格空码分支在源码里**逐行同形**，唯一差别就是这个。锁住它，免得日后有人
/// 「统一两块重复代码」时把不对称一并抹平。
#[test]
fn raw_code_enter_commit_does_not_append_space() {
    if !has_english_schema() {
        return;
    }
    // 同上：关掉原文候选，否则「无候选」的前提不成立。
    let mut cfg = english_config(true);
    cfg.schema.english.raw_candidate = RawCandidateMode::Off;
    let coord = Coordinator::new_headless(cfg, Some(&data_dir()));

    let nonsense = "qwxzjv";
    type_word(&coord, nonsense);
    assert_eq!(
        coord.debug_candidate_count(),
        0,
        "测试前提失效：{nonsense} 现在有候选了"
    );

    let text = commit_text(&coord.handle_key_event(&key_event(VK_RETURN, EVENT_KEY_DOWN)));
    assert_eq!(text, nonsense, "回车上屏原码不得补空格（终结性动作）");
}

/// **非英文方案下不得补空格**——即使开关是开的。
///
/// 这是整组里最关键的一条：`english_appends_space` 的两个条件（候选来源是英文、当前方案
/// 是英文方案）查的**不是同一件事**，而后者极易被当成冗余判断删掉。删掉后临时英文、混输、
/// 快捷输入里插一个英文词都会莫名多出空格，且英文方案自身的测试一条都跑不到。
#[test]
fn non_english_schema_never_appends_space() {
    if !has_english_schema() {
        return;
    }
    let mut cfg = english_config(true);
    cfg.schema.active = "wubi86".into(); // 开关照开，只换方案
    let coord = Coordinator::new_headless(cfg, Some(&data_dir()));
    assert_eq!(coord.active_schema_id(), "wubi86");

    type_word(&coord, "aaaa");
    let top = coord.debug_page_texts().first().cloned().expect("应有候选");

    let text = commit_text(&coord.handle_key_event(&key_event(VK_SPACE, EVENT_KEY_DOWN)));
    assert_eq!(text, top, "非英文方案下即便开关为真也不得补空格");
}

/// IPC 强制提交通路（DLL 侧 TSF 排水 / 顶码延迟提交）同样补空格。
///
/// 这是一条**独立于按键路径**的上屏通路：只改 `commit_selected` 会得到「键盘空格补了、
/// 排水路径没补」的间歇性不一致——正是本仓多次栽过的「上屏有多条通路，开关必须处处接」。
#[test]
fn commit_request_space_appends_space() {
    if !has_english_schema() {
        return;
    }
    let coord = Coordinator::new_headless(english_config(true), Some(&data_dir()));
    type_word(&coord, "hel");
    let top = coord.debug_page_texts().first().cloned().expect("应有候选");

    let res = coord
        .handle_commit_request(&CommitRequestData {
            barrier_seq: 1,
            trigger_key: VK_SPACE as u16,
            modifiers: 0,
            input_buffer: "hel".into(),
        })
        .expect("有输入缓冲时应返回上屏结果");
    assert_eq!(res.text, format!("{top} "), "IPC 空格提交也应补空格");
}

/// IPC 通路的回车分支不补——与按键路径的回车同口径。
#[test]
fn commit_request_enter_does_not_append_space() {
    if !has_english_schema() {
        return;
    }
    let coord = Coordinator::new_headless(english_config(true), Some(&data_dir()));
    let nonsense = "qwxzjv";
    type_word(&coord, nonsense);

    let res = coord
        .handle_commit_request(&CommitRequestData {
            barrier_seq: 1,
            trigger_key: VK_RETURN as u16,
            modifiers: 0,
            input_buffer: nonsense.into(),
        })
        .expect("有输入缓冲时应返回上屏结果");
    assert_eq!(res.text, nonsense, "IPC 回车提交不得补空格");
}

/// 补的空格**不得**进入词频记账键。
///
/// 记账写入端若把 `"hello "` 写进 FREQ 表，读取端按候选文本 `"hello"` 查将永远查不中——
/// 用户选了词却毫无变化，且写入本身成功、任何常规测试都不会失败（本仓已有三处这样的
/// 孤儿键漏网史）。判据是「选过一次后该词升到首位」：记账键一旦带上尾空格，读取端查不中，
/// 位置纹丝不动。
///
/// ⚠️ **必须用 `new_headless_with_store`**：`new_headless` 的 store 是 `None`，词频无处可写、
/// 读取端也无从查起——用它写这条测试，断言失败与否和尾空格毫无关系。
#[test]
fn appended_space_does_not_pollute_freq_key() {
    if !has_english_schema() {
        return;
    }
    let path = std::env::temp_dir().join("wind_english_commit_space_freq.redb");
    let _ = std::fs::remove_file(&path);
    let store = Arc::new(wind_store::Store::open(&path).expect("建 store 失败"));

    let mut cfg = english_config(true);
    cfg.schema.english.frequency.enabled = true;
    cfg.schema.english.frequency.strategy = "top".into(); // 一次到顶，位次变化最好判读
    let coord = Coordinator::new_headless_with_store(cfg, Some(&data_dir()), store);

    type_word(&coord, "hel");
    let page = coord.debug_page_texts();
    assert!(page.len() >= 3, "需原文 + 至少两个词库候选，实际 {page:?}");
    // page[0] 恒是所打原文（raw_candidate 默认开），词库段从 1 开始。
    let second = page[2].clone();

    // 数字键 3 选中词库段的第二条（带空格上屏）
    let text = commit_text(&coord.handle_key_event(&key_event(VK_3, EVENT_KEY_DOWN)));
    assert_eq!(text, format!("{second} "), "选中时应补空格");

    // 再打一次同样的码：记账键若干净，该词应升到**词库段**首位。
    //
    // ⚠️ 断言落在 `after[1]` 而不是 `after[0]`——首位恒是原文，调频只在词库段内部生效
    // （原文与变形被 `split_off(dict_start)` 挡在重排之外，见设计文档 §5.2）。
    type_word(&coord, "hel");
    let after = coord.debug_page_texts();
    assert_eq!(
        after.first().map(String::as_str),
        Some("hel"),
        "首位恒是所打原文"
    );
    assert_eq!(
        after.get(1),
        Some(&second),
        "选过的词应因调频升到词库段首位；纹丝不动说明记账键被尾空格污染成了孤儿键。实际候选: {after:?}"
    );
    let _ = std::fs::remove_file(&path);
}

/// 收尾：keyup 不应产生额外上屏（防止补空格逻辑被误接到 keyup 路径）。
#[test]
fn keyup_does_not_emit_extra_space() {
    if !has_english_schema() {
        return;
    }
    let coord = Coordinator::new_headless(english_config(true), Some(&data_dir()));

    type_word(&coord, "hel");
    coord.handle_key_event(&key_event(VK_SPACE, EVENT_KEY_DOWN));
    let up = coord.handle_key_event(&key_event(VK_SPACE, EVENT_KEY_UP));
    assert!(
        !matches!(up, KeyAction::InsertText { .. }),
        "空格 keyup 不应再次上屏，实际: {up:?}"
    );
}
