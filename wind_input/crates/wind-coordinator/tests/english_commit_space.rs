//! 英文方案「上屏后自动补空格」（`schema.english.commit_space`）端到端测试。
//!
//! 英文上屏有多条通路：`commit_selected`（六类选中触发汇于一处）、空格上屏原码、IPC 强制提交
//! 补空格；回车上屏原码、标点顶屏刻意不补。断言重心是逐条通路各自补没补，少测一条，重构时
//! 就可能把某条通路悄悄改回不生效。
//!
//! 词典缺失时整族静默跳过（判据是耗时 0.00s）。worktree 中需自备 `build_dev` 链接。

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

/// 英文方案就绪判据：方案定义与词库都要在（词库由构建 assemble 注入，不进 git）。
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
        seq_char: 0,
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
/// 断言相对关系（首选文本 + 一个空格）而非硬编码 `"hello "`，免得词库排序变动误报。
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

/// 首字母大写时选中首选上屏，同样要补空格。
///
/// 此时首选是 `english_head_candidates` 构造的原文候选（`source` 为 `None`），
/// `english_appends_space` 只能靠 `text == input` 命中；调用点的 `input` 须是保留大小写的
/// `cased_or_buffer(...)`，传全小写的 `state.input_buffer` 就永不相等。触发开关是
/// `raw_candidate`（默认开），不是 `case_follow_input`。
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

/// 选中大小写变形候选上屏，同样要补空格。
///
/// 变形（`case_variants`，英文方案出厂关）与原文同属 `english_head_candidates` 构造的头部
/// 候选，`source` 为 `None`；判据第二分支只认「与原码逐字节相等」，于是原文补、变形不补。
/// 用词库里没有的串，头部三条的位次就是确定的：`[原文, 首字母大写, 全大写]`。
#[test]
fn case_variant_select_appends_space() {
    if !has_english_schema() {
        return;
    }
    let mut cfg = english_config(true);
    cfg.schema.english.case_variants = true;
    let coord = Coordinator::new_headless(cfg, Some(&data_dir()));

    type_word(&coord, "qzxv");
    let page = coord.debug_page_texts();
    assert_eq!(
        page.get(..3),
        Some(&["qzxv".to_string(), "Qzxv".to_string(), "QZXV".to_string()][..]),
        "前提：头部应为原文 + 两条变形，实际 {page:?}"
    );

    let text = commit_text(&coord.handle_key_event(&key_event(VK_2, EVENT_KEY_DOWN)));
    assert_eq!(text, "Qzxv ", "选首字母大写变形后应补一个空格");

    type_word(&coord, "qzxv");
    let text = commit_text(&coord.handle_key_event(&key_event(VK_3, EVENT_KEY_DOWN)));
    assert_eq!(text, "QZXV ", "选全大写变形后应补一个空格");
}

/// CapsLock 全大写档下选首选（原文头部候选被整列套形成全大写）同样补空格。
///
/// 档位投影作用于含头部候选的整列：原文 `qzxv` 显示为 `QZXV`、`source` 仍为 `None`，
/// 走的是与变形候选同一条判据。钉住它，免得有人以为档位候选恒是词库来源。
#[test]
fn capslock_upper_tier_select_appends_space() {
    if !has_english_schema() {
        return;
    }
    let mut cfg = english_config(true);
    cfg.input.english_case_cycle_key = "capslock".into();
    let coord = Coordinator::new_headless(cfg, Some(&data_dir()));

    type_word(&coord, "qzxv");
    assert!(coord.debug_cycle_english_case(), "前提：应切到全大写档");
    let top = coord.debug_page_texts().first().cloned().expect("应有候选");
    assert_eq!(top, "QZXV", "前提：全大写档首选应是套形后的原文");

    let text = commit_text(&coord.handle_key_event(&key_event(VK_SPACE, EVENT_KEY_DOWN)));
    assert_eq!(text, "QZXV ", "全大写档选首选后应补一个空格");
}

/// 反向对照：开关关闭时不得补空格。没有它，「恒补空格」的实现也能让上面几条通过。
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
/// 空格与数字键目前在 `commit_selected` 内汇于同一分支；日后按触发键分流时这条会失败。
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

/// 打词库里没有的词，空格上屏原码 → 也补空格（与选中候选一致）。
///
/// 走 `VK_SPACE` 空码分支，没有候选可依，判据是方案口径 `english_space_enabled`。
/// 必须显式关掉 `raw_candidate`：否则原文作为首候选在列表里，走不到这条兜底出口
/// （有原文候选时见 `tests/english_head_candidates.rs`）。
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

/// 首字母大写时，「空格上屏原码」的空码兜底分支照样补空格。
///
/// 这条不经候选级判据（`message_handler` 的无候选兜底只看 `english_space_enabled_in`），
/// 选中路径的大写场景见 `space_select_appends_space_when_capitalized`。钉住这条通路对大小写
/// 不敏感，免得有人把候选级判据塞进来。
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

/// 回车上屏原码不补空格：刻意的不对称，不是漏接。
///
/// 回车分支与空格空码分支逐行同形，合并重复代码时别把这个差别抹平。
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

/// 非英文方案下不得补空格，即使开关是开的。
///
/// `english_appends_space` 的两个条件（候选来源是英文、当前方案是英文方案）不是一回事，后者
/// 易被当冗余删掉；删掉后临时英文、混输、快捷输入里的英文词都会多出空格。
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
/// 这条通路独立于按键路径：只改 `commit_selected` 会得到「键盘空格补了、排水路径没补」的
/// 间歇性不一致。
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

/// 补的空格不得进入词频记账键。
///
/// 记账键带尾空格时写入成功、读取端按 `"hello"` 永远查不中（孤儿键）。判据是「选过一次后
/// 该词升到首位」。必须用 `new_headless_with_store`：`new_headless` 的 store 是 `None`。
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

    // 记账键若干净，该词应升到词库段首位，即 `after[1]`：首位恒是原文，原文与变形被
    // `split_off(dict_start)` 挡在重排之外。
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
