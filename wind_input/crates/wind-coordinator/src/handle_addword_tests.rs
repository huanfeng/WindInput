//! 快捷加词状态机单元测试：无头 Coordinator + 临时 store，覆盖纯逻辑
//! （字符还原/词长调整/确认写库）。编码计算依赖引擎，headless 下为空，
//! 故写库测试手动注入 add_word_code。
use super::{ADD_WORD_MAX_LEN, trim_segs_start};
use crate::coordinator::{CommittedSeg, Coordinator};
use std::sync::Arc;
use wind_config::Config;
use wind_keys::keymap;
use wind_store::Store;

/// 自提交打点必须覆盖一切真落屏的返回变体，不只 `InsertText` 那两种。
///
/// 漏打一种（如满码自动上屏的 `CommitThenDeferComposition`），末端表现是长按打字时
/// 候选窗一次都不显示，链路见 docs/architecture/caret-position-host-compat-notes.md 的
/// 「日志判据速查」第 8 条。
#[test]
fn self_commit_is_marked_for_every_landed_variant() {
    use wind_bridge::handler::KeyAction;
    let landed: Vec<KeyAction> = vec![
        KeyAction::InsertText {
            text: "大".into(),
            new_composition: None,
            mode_changed: false,
            chinese_mode: true,
            has_new_composition: false,
        },
        KeyAction::InsertTextWithCursor {
            text: "大".into(),
            cursor_offset: 0,
        },
        KeyAction::ReplaceBackward {
            count: 1,
            text: "大".into(),
        },
        KeyAction::CommitAndHoldComposition {
            commit_text: "大".into(),
            hold_text: "d".into(),
            timeout_ms: 150,
        },
        KeyAction::CommitThenDeferComposition {
            commit_text: "大".into(),
            deferred_composition: "d".into(),
            timeout_ms: 150,
        },
    ];
    for action in landed {
        let c = Coordinator::new_headless(Config::default(), None);
        assert!(c.last_self_commit.lock().unwrap().is_none(), "初始应为空");
        c.note_commit_action(&action);
        assert!(
            c.last_self_commit.lock().unwrap().is_some(),
            "落屏变体必须打点，否则它引发的 SelectionChanged 会被误判成用户移动光标：{action:?}"
        );
    }
}

/// 反向对照：尚未落屏的组合更新不得打点——否则回声宽限期会被无谓刷新，
/// 用户真正移动光标时反而识别不出来（那正是 `caret_cache_verified` 要清位的时机）。
#[test]
fn self_commit_is_not_marked_before_text_lands() {
    use wind_bridge::handler::KeyAction;
    let c = Coordinator::new_headless(Config::default(), None);
    c.note_commit_action(&KeyAction::UpdateComposition {
        text: "d".into(),
        caret_pos: 1,
    });
    assert!(
        c.last_self_commit.lock().unwrap().is_none(),
        "组合区更新还没落屏，不该算作自提交"
    );
}

/// 英文取码 = 单词本身的小写；三类非法输入取空码（让加词中止）。
///
/// 「含空白」是重点：带空格的串当 key 前缀查询永远命中不到，加进去的词打不出来。
#[test]
fn english_add_word_code_is_lowercased_word() {
    let f = Coordinator::english_add_word_code;
    assert_eq!(f("Hello"), ("hello".to_string(), 0));
    assert_eq!(f("WindInput"), ("windinput".to_string(), 0));
    // 连字符/下划线/数字都是英文标识符里的常见成分，只要还有字母就放行。
    assert_eq!(f("well-known"), ("well-known".to_string(), 0));
    assert_eq!(f("utf8"), ("utf8".to_string(), 0));

    // 空白 → 空码（前缀查询命中不到，加了等于没加）
    assert_eq!(f("thank you").0, "", "带空格的码会让前缀查询永远查不到");
    // 非 ASCII → 空码（英文词库的码空间是 ASCII）
    assert_eq!(f("你好").0, "");
    assert_eq!(f("café").0, "");
    // 一个字母都没有 → 空码（纯数字/纯符号不是英文词）
    assert_eq!(f("123").0, "");
    assert_eq!(f("---").0, "");
    assert_eq!(f("").0, "");
}

fn coord(tag: &str) -> Arc<Coordinator> {
    coord_with_clip(tag, "")
}

/// 假剪贴板：`clipboard_get_text` 恒返回构造时给的串，并记下被读了几次。
///
/// 每个测试协调器都必须注入它（`coord` 默认注入空串），否则默认 feature `desktop-ui`
/// 会读开发机真实剪贴板，结果随剪贴板内容漂移。
///
/// 计数是惰性读取的唯一守门：读一次最坏 40ms 且在按键线程上，多读只会让 Ctrl+= 卡一下。
struct MockClip(String, Arc<std::sync::atomic::AtomicUsize>);
impl crate::host_services::HostServices for MockClip {
    fn clipboard_get_text(&self) -> anyhow::Result<String> {
        self.1.fetch_add(1, Ordering::Relaxed);
        Ok(self.0.clone())
    }
}

fn coord_with_clip(tag: &str, clip: &str) -> Arc<Coordinator> {
    coord_counting_clip(tag, clip).0
}

/// 同 [`coord_with_clip`]，另外交出剪贴板读取次数的计数器。
fn coord_counting_clip(
    tag: &str,
    clip: &str,
) -> (Arc<Coordinator>, Arc<std::sync::atomic::AtomicUsize>) {
    let path = std::env::temp_dir().join(format!("wind_addword_{tag}.redb"));
    let _ = std::fs::remove_file(&path);
    let store = Arc::new(Store::open(&path).unwrap());
    let c = Coordinator::new_headless_with_store(Config::default(), None, store);
    let reads = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    c.set_host_services(Arc::new(MockClip(clip.to_string(), reads.clone())));
    (c, reads)
}

/// 构造一个 key_down 事件（加词模式的按键分派只看 key_code 与修饰位）。
fn key(vk: u32, mods: u32) -> wind_bridge::handler::KeyEventData {
    wind_bridge::handler::KeyEventData {
        key_code: vk,
        scan_code: 0,
        modifiers: mods,
        event_type: wind_ipc::protocol::EVENT_KEY_DOWN,
        toggles: 0,
        event_seq: 0,
        prev_char: 0,
        seq_char: 0,
    }
}

/// 按时间序模拟上屏：最早先入，最新最后（push_front 保证最新在前，对齐运行时）。
fn push_commits(c: &Coordinator, items: &[&str]) {
    let mut h = c.recent_commits.lock().unwrap();
    for it in items {
        h.push_front(it.to_string());
    }
}

use std::sync::atomic::Ordering;
use wind_bridge::handler::{KeyAction, MessageHandler};

/// 撤销上屏计数（`last_commit_len`）随「同步落屏」按键更新：中文整词记字符数、
/// 英文/标点逐键覆盖回 1（防残留旧中文计数误删）、emoji 按 UTF-16 单元、组合态不动。
#[test]
fn undo_commit_len_tracks_last_commit() {
    let c = coord("undo_len");
    assert_eq!(c.last_commit_len.load(Ordering::Relaxed), 1, "默认删 1");

    c.note_commit_action(&Coordinator::commit_action("你好".into(), true));
    assert_eq!(c.last_commit_len.load(Ordering::Relaxed), 2, "中文整词记 2");

    // 英文逐键上屏刷回 1：这一步顶掉上面的中文计数，正是「敲 abc 后 undo 不误删你好」的关键。
    c.note_commit_action(&Coordinator::commit_action("a".into(), false));
    assert_eq!(c.last_commit_len.load(Ordering::Relaxed), 1, "英文覆盖为 1");

    // emoji：UTF-16 surrogate pair 计 2 单元，与 TSF/macOS 删除量纲一致。
    c.note_commit_action(&Coordinator::commit_action("😀".into(), true));
    assert_eq!(
        c.last_commit_len.load(Ordering::Relaxed),
        2,
        "emoji 记 2 单元"
    );

    // 组合态（尚未落屏）不动计数。
    c.note_commit_action(&KeyAction::PassThrough);
    assert_eq!(c.last_commit_len.load(Ordering::Relaxed), 2, "组合态不覆盖");

    // 智能标点替换：按替换后光标前文本长度计。
    c.note_commit_action(&KeyAction::ReplaceBackward {
        count: 2,
        text: "—".into(),
    });
    assert_eq!(
        c.last_commit_len.load(Ordering::Relaxed),
        1,
        "替换后记新符号长"
    );
}

/// 上屏之后有键透传给宿主（DLL 经下一按的 `TOGGLE_PASSTHROUGH_KEY` 报告）：光标前最后一段
/// 已是宿主自己出的字，撤销计数退化为 1——否则「你好」后打个 0 再撤销，删的是「好0」。
#[test]
fn host_typed_key_resets_undo_commit_len() {
    let c = coord("undo_len_host_key");
    c.note_commit_action(&Coordinator::commit_action("你好".into(), true));
    assert_eq!(c.last_commit_len.load(Ordering::Relaxed), 2);
    let shift = |toggles| wind_bridge::handler::KeyEventData {
        key_code: 0x10, // Shift：本身什么都不做，只为把透传位带进来
        scan_code: 0,
        modifiers: 0,
        event_type: wind_ipc::protocol::EVENT_KEY_DOWN,
        toggles,
        event_seq: 0,
        prev_char: 0,
        seq_char: 0,
    };
    // 反向：不带位的按键不动计数（防「每个 keydown 都复位」）。
    c.handle_key_event_policed(&shift(0));
    assert_eq!(
        c.last_commit_len.load(Ordering::Relaxed),
        2,
        "无透传位不复位"
    );
    c.handle_key_event_policed(&shift(wind_ipc::protocol::TOGGLE_PASSTHROUGH_KEY));
    assert_eq!(
        c.last_commit_len.load(Ordering::Relaxed),
        1,
        "透传位到达后撤销只删 1"
    );
}

/// 焦点变化复位撤销计数：换窗/换文本框后光标前已非「刚上屏那段」，退化删 1。
#[test]
fn focus_lost_resets_undo_commit_len() {
    let c = coord("undo_focus");
    c.note_commit_action(&Coordinator::commit_action("世界".into(), true));
    assert_eq!(c.last_commit_len.load(Ordering::Relaxed), 2);
    c.handle_focus_lost(0, wind_bridge::handler::FocusLostReason::Thread);
    assert_eq!(
        c.last_commit_len.load(Ordering::Relaxed),
        1,
        "失焦后退化删 1"
    );
}

/// 打字中（输入缓冲非空）触发 undo 应提前返回、不消耗计数（不 swap）。
#[test]
fn undo_commit_ignored_while_composing() {
    let c = coord("undo_composing");
    c.last_commit_len.store(5, Ordering::Relaxed);
    c.state.lock().unwrap().input_buffer = "abc".to_string();
    c.cmd_undo_commit();
    assert_eq!(
        c.last_commit_len.load(Ordering::Relaxed),
        5,
        "缓冲非空应忽略，计数不变"
    );
}

/// learn_phrase_on_commit 6a 晋升路径：promote_count=2，造词两次达阈值 → 自动晋升。
#[test]
fn learn_phrase_promotes_at_threshold_via_6a() {
    use wind_candidate::CandidateSource as CS;
    use wind_store::Store;
    let path = std::env::temp_dir().join("wind_addword_promote6a.redb");
    let _ = std::fs::remove_file(&path);
    let store = Arc::new(Store::open(&path).unwrap());

    // 开启自动造词 + 晋升阈值=2。闸门归 [schema.pinyin.auto_learn]——
    // `learn_phrase_on_commit` 的产出恒是拼音词（纯码表在 is_codetable() 处已返回）。
    let mut cfg = Config::default();
    cfg.schema.pinyin.auto_learn.enabled = true;
    cfg.schema.pinyin.auto_learn.promote_count = 2;

    let c = Coordinator::new_headless_with_store(cfg, None, store.clone());

    // 辅助：构造含 2 段的 State 并调用 learn_phrase_on_commit
    let make_state_with_segs = || {
        let mut st = c.state.lock().unwrap();
        // 码表段：raw_code 与 code 同为击键码（无双拼转换）。
        let seg = |code: &str, text: &str| CommittedSeg {
            raw_code: code.to_string(),
            code: code.to_string(),
            text: text.to_string(),
            source: CS::CodeTable,
            boundary: 0,
            learn: None,
        };
        st.committed_segs = vec![seg("aa", "工"), seg("bb", "人")];
        drop(st);
    };

    // 第 1 次造词 → temp count=1，未达阈值
    make_state_with_segs();
    {
        let st = c.state.lock().unwrap();
        c.learn_phrase_on_commit(&st, false); // 分步造词路径，非整句
    }

    let active = c.engine_mgr.active_schema_id();
    let schema = c.engine_mgr.data_schema_id(&active);
    let count1 = store.get_temp_word(&schema, "aabb", "工人").unwrap();
    assert_eq!(count1, Some(1), "第 1 次造词后临时层 count 应为 1");

    // 第 2 次造词 → temp count=2，达阈值 → 自动晋升
    make_state_with_segs();
    {
        let st = c.state.lock().unwrap();
        c.learn_phrase_on_commit(&st, false); // 分步造词路径，非整句
    }

    // 临时层应已删除
    let count2 = store.get_temp_word(&schema, "aabb", "工人").unwrap();
    assert_eq!(count2, None, "晋升后临时层应删除");
    // 用户词层应含晋升的词
    let user = store.get_user_words(&schema, "aabb").unwrap();
    assert!(
        user.iter().any(|r| r.text == "工人"),
        "用户词层应含晋升的词"
    );
    let _ = std::fs::remove_file(&path);
}

#[test]
fn recent_chars_order_and_truncate() {
    let c = coord("recent");
    push_commits(&c, &["你", "好", "世界"]);
    assert_eq!(
        c.add_word_recent_chars(20).iter().collect::<String>(),
        "你好世界"
    );
    assert_eq!(
        c.add_word_recent_chars(2).iter().collect::<String>(),
        "世界"
    );
}

#[test]
fn enter_sets_default_len_and_word() {
    let c = coord("enter");
    push_commits(&c, &["你", "好"]);
    let mut st = c.state.lock().unwrap();
    c.enter_add_word_mode(&mut st);
    assert!(st.add_word_active);
    assert_eq!(st.add_word_len, 2);
    assert_eq!(c.add_word_current_word(&st), "你好");
}

#[test]
fn enter_single_char_caps_len_to_one() {
    let c = coord("single");
    push_commits(&c, &["好"]);
    let mut st = c.state.lock().unwrap();
    c.enter_add_word_mode(&mut st);
    assert_eq!(st.add_word_len, 1);
    assert_eq!(c.add_word_current_word(&st), "好");
}

#[test]
fn enter_no_history_zero_len() {
    let c = coord("empty");
    let mut st = c.state.lock().unwrap();
    c.enter_add_word_mode(&mut st);
    assert!(st.add_word_active);
    assert_eq!(st.add_word_len, 0);
    assert!(st.add_word_code.is_empty());
}

#[test]
fn adjust_length_clamps() {
    let c = coord("adjust");
    push_commits(&c, &["一", "二", "三"]);
    let mut st = c.state.lock().unwrap();
    c.enter_add_word_mode(&mut st);
    assert_eq!(st.add_word_len, 2);
    c.adjust_add_word_length(&mut st, 1);
    assert_eq!(st.add_word_len, 3);
    c.adjust_add_word_length(&mut st, 1); // 上限 = 字符数 3
    assert_eq!(st.add_word_len, 3);
    assert_eq!(c.add_word_current_word(&st), "一二三");
    c.adjust_add_word_length(&mut st, -5); // 下限 1
    assert_eq!(st.add_word_len, 1);
    assert_eq!(c.add_word_current_word(&st), "三");
}

#[test]
fn confirm_empty_code_aborts_without_write() {
    let c = coord("abort");
    push_commits(&c, &["你", "好"]);
    let mut st = c.state.lock().unwrap();
    c.enter_add_word_mode(&mut st);
    assert!(st.add_word_code.is_empty(), "headless 无引擎，编码应为空");
    c.confirm_add_word(&mut st);
    assert!(!st.add_word_active, "确认后应退出加词模式");
    drop(st);
    let schema = c.engine_mgr.active_schema_id();
    let schema = c.engine_mgr.data_schema_id(&schema); // 与写入路径一致
    let store = c.store.as_ref().unwrap();
    // 编码为空时不应写任何用户词；遍历常见空码均无记录。
    assert!(store.get_user_words(&schema, "").unwrap().is_empty());
}

#[test]
fn confirm_with_code_writes_user_word() {
    let c = coord("write");
    push_commits(&c, &["你", "好"]);
    let mut st = c.state.lock().unwrap();
    c.enter_add_word_mode(&mut st);
    st.add_word_code = "nihao".to_string(); // headless 无引擎，手动注入编码
    c.confirm_add_word(&mut st);
    assert!(!st.add_word_active);
    drop(st);
    let schema = c.engine_mgr.active_schema_id();
    let schema = c.engine_mgr.data_schema_id(&schema); // 与写入路径一致（拼音族→"pinyin"；headless 下 ""→""）
    let store = c.store.as_ref().unwrap();
    let recs = store.get_user_words(&schema, "nihao").unwrap();
    assert_eq!(recs.len(), 1, "应写入 1 条用户词");
    assert_eq!(recs[0].text, "你好");
    assert_eq!(recs[0].weight, 1200);
}

#[test]
fn exit_resets_state() {
    let c = coord("exit");
    push_commits(&c, &["你", "好"]);
    let mut st = c.state.lock().unwrap();
    c.enter_add_word_mode(&mut st);
    assert!(st.add_word_active);
    c.exit_add_word_mode(&mut st);
    assert!(!st.add_word_active);
    assert!(st.add_word_chars.is_empty());
    assert_eq!(st.add_word_len, 0);
    assert!(st.add_word_code.is_empty());
}

#[test]
fn trim_segs_keeps_tail_within_max() {
    use wind_candidate::CandidateSource as S;
    let seg = |c: &str, t: &str| CommittedSeg {
        raw_code: c.to_string(),
        code: c.to_string(),
        text: t.to_string(),
        source: S::CodeTable,
        boundary: 0,
        learn: None,
    };
    let segs = vec![seg("aa", "工人"), seg("bb", "们"), seg("cc", "好的")];
    // 总 5 字，max=3 → 从尾部保留 "们"(1)+"好的"(2)=3 字，起始索引 1
    assert_eq!(trim_segs_start(&segs, 3), 1);
    // max=0 不限
    assert_eq!(trim_segs_start(&segs, 0), 0);
    // max=1 装不下末段(2字) → 起始=len（全部裁掉，调用方跳过）
    assert_eq!(trim_segs_start(&segs, 1), 3);
}

/// 加词参数串：空字段跳过（设置端把"空串"与"没传"当同一回事）。
/// 页名由 `open_settings_with` 单独带，故这里只断言参数部分。
#[test]
fn build_page_omits_empty_fields() {
    use crate::handle_menu::build_settings_args;
    let args = |w, c, s| build_settings_args(&[("text", w), ("code", c), ("schema", s)]);
    assert_eq!(
        args("你好", "nihao", "pinyin"),
        "--text=你好 --code=nihao --schema=pinyin"
    );
    assert_eq!(args("", "", ""), "");
    assert_eq!(args("你好", "", "wubi"), "--text=你好 --schema=wubi");
}

/// 含空白的值必须加引号：参数串经 ShellExecuteW 交给设置端后由
/// CommandLineToArgvW 重新切分，不加引号会把 `--text=你 好` 拆成两个 argv，
/// 设置端只收得到 `--text=你`（剪贴板加词的整段文本正是这种值）。
#[test]
fn build_args_quotes_values_with_whitespace() {
    use crate::handle_menu::build_settings_args;
    assert_eq!(
        build_settings_args(&[("text", "hello world"), ("schema", "wubi86")]),
        "--text=\"hello world\" --schema=wubi86"
    );
}

/// 拼音方案下含非汉字的词不取码，加词中止，不留坏数据。
///
/// `gen_pinyin` 的 `filter_map` 静默跳过无读音字符，「你好a」会产出覆盖不全的 `ni hao`；
/// 快捷加词的字符池取最近上屏字符，中英混输下很常见。与纯英文取空码一致。
///
/// 必须用真实词库：headless 无引擎时取码恒为空串，「断言空码」会假绿，故对照组
/// （纯汉字词必须取得出码）不能省。
#[test]
fn pinyin_rejects_non_han_word_and_adds_han_word() {
    let data = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../build_dev/data");
    if !data.join("schemas/pinyin.schema.toml").exists() {
        return;
    }
    let mut cfg = Config::default();
    cfg.schema.available = vec!["pinyin".into()];
    cfg.schema.active = "pinyin".into();
    let path = std::env::temp_dir().join("wind_addword_fallback_flat.redb");
    let _ = std::fs::remove_file(&path);
    let store = Arc::new(Store::open(&path).unwrap());
    let c = Coordinator::new_headless_with_store(cfg, Some(&data), store);

    let sid = c.engine_mgr.active_schema_id();
    let sid = c.engine_mgr.data_schema_id(&sid);

    // ① 含非汉字「你好a」→ 不取码、不写库
    push_commits(&c, &["你", "好", "a"]);
    {
        let mut st = c.state.lock().unwrap();
        c.enter_add_word_mode(&mut st);
        c.adjust_add_word_length(&mut st, 1); // 默认 2（"好a"）→ 3（"你好a"）
        assert_eq!(c.add_word_current_word(&st), "你好a");
        assert!(
            st.add_word_code.is_empty(),
            "含非汉字应取不出码，实际 {:?}",
            st.add_word_code
        );
        c.confirm_add_word(&mut st);
    }
    let store = c.store.as_ref().unwrap();
    assert!(
        store.get_user_words(&sid, "nihao").unwrap().is_empty(),
        "不得把丢了字母的「ni hao → 你好a」写进库"
    );
    assert!(
        store.get_user_words(&sid, "ni hao").unwrap().is_empty(),
        "更不得写出带空格的坏 key"
    );

    // ② 对照组：纯汉字「你好」照常取码入库。
    //    没有这一半，上面的「空码」断言在引擎根本没加载时也会通过——那是假绿。
    push_commits(&c, &["你", "好"]);
    {
        let mut st = c.state.lock().unwrap();
        c.enter_add_word_mode(&mut st);
        assert_eq!(c.add_word_current_word(&st), "你好");
        assert_eq!(st.add_word_code, "nihao", "落库 code 必须扁平、无空格");
        assert_eq!(st.add_word_boundary, 0b101, "音节边界 ni|hao");
        // 展示形态还原得出空格（与设置页「出码」按钮、词库列表同形）
        assert_eq!(c.add_word_display_code(&st), "ni hao");
        c.confirm_add_word(&mut st);
    }
    assert_eq!(
        store.get_user_words(&sid, "nihao").unwrap().len(),
        1,
        "扁平 key 必须查得到——这正是候选查询用的形态"
    );
    let _ = std::fs::remove_file(&path);
}

// ──────────────────────────────────────────────────────────────────────
// 编码的展示形态（带音节空格）——三个加词界面的共用口径
// ──────────────────────────────────────────────────────────────────────

/// 存储域（扁平 key + boundary）→ 展示域（带空格音节码）的转换语义。
/// 拼音方案下少了这一步，Ctrl+Shift+= 的对话框会预填出 `nihao`。
#[test]
fn display_code_restores_syllable_spaces() {
    use super::display_code;
    // 拼音多音节：boundary 的 bit 位是各音节起始字节偏移（ni|hao → {0,2}）
    assert_eq!(display_code("nihao", 0b101), "ni hao");
    assert_eq!(display_code("chongqing", 0b100001), "chong qing");
    // 码表码 boundary 恒 0 → 恒等变换，五笔等方案不受影响
    assert_eq!(display_code("aabb", 0), "aabb");
    // 单音节（0b1）无内部边界可插，与 0 同样原样返回
    assert_eq!(display_code("ni", 0b1), "ni");
    assert_eq!(display_code("", 0), "");
}

/// `add_word_code` 与 `add_word_boundary` 必须成对读出。
///
/// 三个显示/传参点只取扁平 code、丢掉 boundary，音节空格就没了。
#[test]
fn add_word_display_code_reads_boundary_from_state() {
    let c = coord("display_code_state");
    push_commits(&c, &["你", "好"]);
    let mut st = c.state.lock().unwrap();
    c.enter_add_word_mode(&mut st);
    // headless 无引擎，calc_add_word_code 取不出码 → 手动注入引擎会给出的那一对
    st.add_word_code = "nihao".to_string();
    st.add_word_boundary = 0b101;
    assert_eq!(
        c.add_word_display_code(&st),
        "ni hao",
        "展示形态必须带音节空格，与设置页「出码」按钮及词库列表同形"
    );
    // 码表方案（boundary=0）不受影响
    st.add_word_code = "aabb".to_string();
    st.add_word_boundary = 0;
    assert_eq!(c.add_word_display_code(&st), "aabb");
}

/// 带空格的码进参数串必须被引号包住。
///
/// 不加引号 CommandLineToArgvW 会把 `--code=ni hao` 切成两个 argv，设置端只收到 `--code=ni`。
#[test]
fn prefill_args_quote_spaced_code() {
    use super::display_code;
    use crate::handle_menu::build_settings_args;
    let code = display_code("nihao", 0b101);
    assert_eq!(
        build_settings_args(&[("text", "你好"), ("code", &code), ("schema", "pinyin")]),
        "--text=你好 --code=\"ni hao\" --schema=pinyin"
    );
}

// ── 剪贴板加词来源（Ctrl+= 的 Tab 切换 / Ctrl+Shift+= 的默认来源） ──────────

/// Ctrl+= 默认仍是最近输入；Tab 切到剪贴板后整段全选（而非沿用默认 2 字）。
#[test]
fn tab_switches_to_clipboard_and_selects_whole() {
    let c = coord_with_clip("tabclip", "量子纠缠态");
    push_commits(&c, &["你", "好"]);
    let mut st = c.state.lock().unwrap();
    c.enter_add_word_mode(&mut st);
    assert!(!st.add_word_from_clip, "有最近输入时默认来源必须是最近输入");
    assert_eq!(c.add_word_current_word(&st), "你好");

    c.toggle_add_word_source(&mut st);
    assert!(st.add_word_from_clip);
    assert_eq!(st.add_word_len, 5, "剪贴板来源默认全选");
    assert_eq!(c.add_word_current_word(&st), "量子纠缠态");

    // 再按一次切回，长度按最近输入的默认值重置
    c.toggle_add_word_source(&mut st);
    assert!(!st.add_word_from_clip);
    assert_eq!(st.add_word_len, 2);
    assert_eq!(c.add_word_current_word(&st), "你好");
}

/// 剪贴板来源下 ↓ 砍的是尾部——方向搞反会得到「子纠缠态」，且不报任何错。
#[test]
fn clipboard_source_trims_from_tail() {
    let c = coord_with_clip("cliptrim", "量子纠缠态");
    push_commits(&c, &["你", "好"]);
    let mut st = c.state.lock().unwrap();
    c.enter_add_word_mode(&mut st);
    c.toggle_add_word_source(&mut st);
    c.adjust_add_word_length(&mut st, -1);
    assert_eq!(c.add_word_current_word(&st), "量子纠缠");
    c.adjust_add_word_length(&mut st, -1);
    assert_eq!(c.add_word_current_word(&st), "量子纠");
}

/// 最近输入为空、剪贴板有内容 ⇒ 进入时自动落在剪贴板一侧，但仍切得回来。
#[test]
fn enter_auto_lands_on_clipboard_when_no_recent() {
    let c = coord_with_clip("autoclip", "量子纠缠");
    let mut st = c.state.lock().unwrap();
    c.enter_add_word_mode(&mut st);
    assert!(st.add_word_from_clip, "没有最近输入时应直接落在剪贴板");
    assert_eq!(c.add_word_current_word(&st), "量子纠缠");

    // 自动落点不是锁死：Tab 仍能切回（那一侧是空的，面板给空态）
    c.toggle_add_word_source(&mut st);
    assert!(!st.add_word_from_clip);
    assert_eq!(st.add_word_len, 0);
}

/// 反向不自动：最近输入有内容时，剪贴板可不可用都不改变默认来源。
#[test]
fn enter_keeps_recent_when_available() {
    for (tag, clip) in [("keepempty", ""), ("keepfull", "剪贴内容")] {
        let c = coord_with_clip(tag, clip);
        push_commits(&c, &["你", "好"]);
        let mut st = c.state.lock().unwrap();
        c.enter_add_word_mode(&mut st);
        assert!(!st.add_word_from_clip, "{tag}: 有最近输入就该停在最近输入");
    }
}

/// 剪贴板不可用时 Tab 照样切得过去，切过去是剪贴板侧的空态（不是原地不动）。
///
/// 两个来源必须对称，不能「空则不许切」，否则 Tab 时灵时不灵。
#[test]
fn tab_switches_even_when_clipboard_unusable() {
    // 空 / 全空白 / 多行：三种都归到「剪贴板无可用内容」这一个可见状态。
    // 超长不在此列——截取前 ADD_WORD_MAX_LEN 字，见
    // `overlong_clipboard_is_truncated_not_rejected`。
    for (tag, clip) in [
        ("clipempty", "".to_string()),
        ("clipblank", "   \t ".to_string()),
        ("clipmulti", "第一行\n第二行".to_string()),
    ] {
        let c = coord_with_clip(tag, &clip);
        push_commits(&c, &["你", "好"]);
        let mut st = c.state.lock().unwrap();
        c.enter_add_word_mode(&mut st);
        // 这里不断言剪贴板池：惰性读取后，有最近上屏时此刻根本还没读过剪贴板
        // （那正是 `entering_with_recent_input_never_reads_clipboard` 守着的事）。
        // 「不合规 ⇒ 空池」由下面切过去之后的 `add_word_len == 0` 验。
        c.toggle_add_word_source(&mut st);
        assert!(st.add_word_from_clip, "{tag}: 空的一侧也必须切得过去");
        assert_eq!(st.add_word_len, 0, "{tag}: 空池无可选词");
        let rows = c.add_word_panel_rows(&st);
        assert_eq!(rows[0].text, "快捷加词 · 剪贴板");
        assert_eq!(rows[1].text, "剪贴板无可用内容");

        c.toggle_add_word_source(&mut st);
        assert_eq!(c.add_word_current_word(&st), "你好", "{tag}: 切得回来");
    }
}

/// 恰好等于上限的剪贴板仍可用（守卫是 `>` 不是 `>=`，差一即整条功能对长词失效）。
#[test]
fn clipboard_at_max_len_is_usable() {
    let word = "字".repeat(ADD_WORD_MAX_LEN);
    let c = coord_with_clip("clipmax", &word);
    push_commits(&c, &["你", "好"]);
    let mut st = c.state.lock().unwrap();
    c.enter_add_word_mode(&mut st);
    c.toggle_add_word_source(&mut st);
    assert!(st.add_word_from_clip);
    assert_eq!(c.add_word_current_word(&st), word);
}

/// 剪贴板池在进入模式时定格：Tab 用的是 State 里的池，不再现读剪贴板
/// （读一次最坏 40ms，见 clipboard_add_word_chars）。
#[test]
fn tab_uses_frozen_pool_not_a_fresh_read() {
    let c = coord_with_clip("clipfrozen", "定格");
    push_commits(&c, &["你", "好"]);
    let mut st = c.state.lock().unwrap();
    c.enter_add_word_mode(&mut st);
    st.add_word_clip = Some("改过".chars().collect());
    c.toggle_add_word_source(&mut st);
    assert_eq!(
        c.add_word_current_word(&st),
        "改过",
        "Tab 必须用 State 里定格的池，而不是现读剪贴板"
    );
}

/// 退出加词模式必须连剪贴板池与来源一起清——留着会让下一次进入时首屏直接显示
/// 上一次的剪贴板内容（`enter` 虽会重读池，但 `from_clip` 残留为真即错位）。
#[test]
fn exit_clears_clipboard_source() {
    let c = coord_with_clip("clipexit", "残留");
    push_commits(&c, &["你", "好"]);
    let mut st = c.state.lock().unwrap();
    c.enter_add_word_mode(&mut st);
    c.toggle_add_word_source(&mut st);
    assert!(st.add_word_from_clip);
    c.exit_add_word_mode(&mut st);
    assert!(!st.add_word_from_clip, "退出必须复位来源");
    assert!(
        st.add_word_clip.is_none(),
        "退出必须把剪贴板池复位成「没读过」"
    );
}

/// 超长剪贴板截断取前 N 字，不整段作废：复制一句话时要的词往往在开头。
/// 截断方向与剪贴板来源「取开头 N 字、↓ 砍尾」一致。
#[test]
fn overlong_clipboard_is_truncated_not_rejected() {
    let long: String = "量子纠缠态的测量与坍缩过程很复杂".to_string();
    assert!(long.chars().count() > ADD_WORD_MAX_LEN, "夹具本身要够长");
    let head: String = long.chars().take(ADD_WORD_MAX_LEN).collect();

    // Ctrl+= 面板：Tab 切过去拿到的是前 N 字
    let c = coord_with_clip("cliptrunc", &long);
    push_commits(&c, &["你", "好"]);
    let mut st = c.state.lock().unwrap();
    c.enter_add_word_mode(&mut st);
    c.toggle_add_word_source(&mut st);
    assert_eq!(c.add_word_current_word(&st), head);
    assert_eq!(st.add_word_len, ADD_WORD_MAX_LEN, "截断后仍是全选");
    drop(st);

    // Ctrl+Shift+= 直开：同样截断，而不是回退最近上屏
    let (word, _) = c.add_word_prefill_from_history();
    assert_eq!(word, head, "超长剪贴板须截断取用，不得回退最近输入");
}

/// 有最近上屏时进入加词模式一次剪贴板都不读。
///
/// 读一次最坏 40ms，且在按键线程上、与 C++ 建立占位 composition 同一拍；多数人进来是加
/// 刚打的字、不按 Tab。多读不会报错只会卡，只有这条计数断言拦得住。
#[test]
fn entering_with_recent_input_never_reads_clipboard() {
    let (c, reads) = coord_counting_clip("lazyenter", "剪贴内容");
    push_commits(&c, &["你", "好"]);
    let mut st = c.state.lock().unwrap();
    c.enter_add_word_mode(&mut st);
    assert_eq!(
        reads.load(Ordering::Relaxed),
        0,
        "有最近上屏时进入加词模式不得读剪贴板"
    );
    // ↑↓ 调长度、确认前的重算同样不该读
    c.adjust_add_word_length(&mut st, 1);
    c.adjust_add_word_length(&mut st, -1);
    assert_eq!(reads.load(Ordering::Relaxed), 0, "调词长不得读剪贴板");
}

/// 没有最近上屏时才读一次（那时要靠它决定自动落点），且此后不再重复读。
#[test]
fn clipboard_is_read_once_and_reused() {
    let (c, reads) = coord_counting_clip("lazyonce", "量子纠缠");
    let mut st = c.state.lock().unwrap();
    c.enter_add_word_mode(&mut st);
    assert_eq!(reads.load(Ordering::Relaxed), 1, "自动落点判断读一次");
    assert!(st.add_word_from_clip);

    // Tab 来回切：池子已在 State 里，不得再读
    c.toggle_add_word_source(&mut st);
    c.toggle_add_word_source(&mut st);
    assert_eq!(reads.load(Ordering::Relaxed), 1, "Tab 复用定格池，不得重读");
    assert_eq!(c.add_word_current_word(&st), "量子纠缠");
}

/// 用户按 Tab 主动切过去时才读——那一次的等待是他要的。
#[test]
fn tab_reads_clipboard_on_demand() {
    let (c, reads) = coord_counting_clip("lazytab", "量子纠缠");
    push_commits(&c, &["你", "好"]);
    let mut st = c.state.lock().unwrap();
    c.enter_add_word_mode(&mut st);
    assert_eq!(reads.load(Ordering::Relaxed), 0);

    c.toggle_add_word_source(&mut st);
    assert_eq!(reads.load(Ordering::Relaxed), 1, "切到剪贴板时读一次");
    assert_eq!(c.add_word_current_word(&st), "量子纠缠");

    // 切回最近上屏不读，再切回来也不读（池子已在 State 里）
    c.toggle_add_word_source(&mut st);
    c.toggle_add_word_source(&mut st);
    assert_eq!(reads.load(Ordering::Relaxed), 1, "只读那一次");
}

/// ESC 在任何一种状态下都必须退出加词模式。
///
/// ESC 走 `handle_add_word_key` 的第一条臂，被前面新增分支抢走（或 `add_word_active`
/// 被别处清掉）时表现为面板还在、按键没反应。
#[test]
fn escape_exits_from_every_state() {
    // ① 最近上屏来源 ② 剪贴板来源 ③ 两侧都空的空态
    let cases: [(&str, &str, &[&str], bool); 3] = [
        ("esc_recent", "剪贴内容", &["你", "好"], false),
        ("esc_clip", "量子纠缠", &["你", "好"], true),
        ("esc_empty", "", &[], false),
    ];
    for (tag, clip, commits, switch) in cases {
        let c = coord_with_clip(tag, clip);
        push_commits(&c, commits);
        let mut st = c.state.lock().unwrap();
        c.enter_add_word_mode(&mut st);
        if switch {
            c.toggle_add_word_source(&mut st);
        }
        assert!(st.add_word_active, "{tag}: 前提——已进入加词模式");

        let act = c.handle_add_word_key(&mut st, &key(keymap::VK_ESCAPE, 0));
        assert!(
            matches!(act, KeyAction::ClearComposition),
            "{tag}: ESC 必须清掉占位组合，否则宿主那边的组合区留着不动"
        );
        assert!(!st.add_word_active, "{tag}: ESC 必须退出加词模式");
        assert!(st.add_word_clip.is_none(), "{tag}: 退出须复位剪贴板池");
        assert!(!st.add_word_from_clip, "{tag}: 退出须复位来源");
    }
}

/// 退格与 ESC 同一条臂（历史约定：加词面板里退格也是「取消」）。
#[test]
fn backspace_exits_like_escape() {
    let c = coord_with_clip("escback", "");
    push_commits(&c, &["你", "好"]);
    let mut st = c.state.lock().unwrap();
    c.enter_add_word_mode(&mut st);
    c.handle_add_word_key(&mut st, &key(keymap::VK_BACK, 0));
    assert!(!st.add_word_active, "退格应与 ESC 同样退出");
}

/// 面板是三行，且操作提示落在 `comment` 而非 `text`。
///
/// 放 text 会用候选正文色，面板上最不重要的一行反而最显眼。
#[test]
fn panel_hint_is_a_dim_third_row() {
    let c = coord_with_clip("panelrows", "剪贴内容");
    push_commits(&c, &["你", "好"]);
    let mut st = c.state.lock().unwrap();
    c.enter_add_word_mode(&mut st);
    let rows = c.add_word_panel_rows(&st);
    assert_eq!(rows.len(), 3, "标题 / 词与编码 / 提示");
    assert_eq!(rows[0].text, "快捷加词 · 最近输入");
    assert!(rows[0].comment.is_empty(), "标题行不再挂提示，否则又被撑宽");
    assert_eq!(rows[1].text, "你好");
    assert!(
        rows[2].text.is_empty(),
        "提示必须留在 comment 走注释色，放 text 会比标题还抢眼"
    );
    assert!(rows[2].comment.as_str().contains("Tab切换来源"));
    assert!(rows.iter().all(|r| r.no_index), "三行都不显序号");
}

/// 面板在两个来源下长得一样：来源后缀与 Tab 提示恒显示，与哪一侧有没有内容无关。
#[test]
fn panel_looks_the_same_on_both_sources() {
    // 两侧都空：仍带来源后缀、仍提示 Tab
    let c = coord("panelbothempty");
    let mut st = c.state.lock().unwrap();
    c.enter_add_word_mode(&mut st);
    let rows = c.add_word_panel_rows(&st);
    assert_eq!(rows[0].text, "快捷加词 · 最近输入", "空也要标来源");
    assert_eq!(rows[1].text, "无最近输入");
    assert_eq!(rows[2].comment.as_str(), "Tab切换来源  Esc关闭");

    c.toggle_add_word_source(&mut st);
    let rows = c.add_word_panel_rows(&st);
    assert_eq!(rows[0].text, "快捷加词 · 剪贴板");
    assert_eq!(rows[1].text, "剪贴板无可用内容");
    assert_eq!(
        rows[1].comment.as_str(),
        "需要单行文本",
        "空态须交代准入条件，否则用户以为复制没生效"
    );
    assert_eq!(
        rows[2].comment.as_str(),
        "Tab切换来源  Esc关闭",
        "两侧提示同形"
    );
}

/// Ctrl+Shift+= 的来源优先级与 Ctrl+= 相反：剪贴板优先、整段作词。
#[test]
fn from_history_prefers_clipboard() {
    let c = coord_with_clip("histclip", "量子纠缠");
    push_commits(&c, &["你", "好"]);
    let (word, _) = c.add_word_prefill_from_history();
    assert_eq!(word, "量子纠缠", "剪贴板可用时必须优先于最近输入");
}

/// 剪贴板不可用则回退最近输入（默认 2 字）——「如果剪贴板为空，则以最新输入的数据」。
#[test]
fn from_history_falls_back_to_recent() {
    // 超长不在此列：它已改为截断取用，不再回退（见
    // `overlong_clipboard_is_truncated_not_rejected`）。
    for (tag, clip) in [
        ("histempty", "".to_string()),
        ("histmulti", "一行\n二行".to_string()),
    ] {
        let c = coord_with_clip(tag, &clip);
        push_commits(&c, &["世", "界", "和", "平"]);
        let (word, _) = c.add_word_prefill_from_history();
        assert_eq!(word, "和平", "{tag}: 剪贴板不可用须回退最近输入末尾 2 字");
    }
}

/// 两个来源都没内容时预填为空（设置端加词界面开着但不填）。
#[test]
fn from_history_empty_when_no_source() {
    let c = coord_with_clip("histnone", "");
    let (word, code) = c.add_word_prefill_from_history();
    assert!(word.is_empty() && code.is_empty());
}

#[test]
fn from_history_does_not_enter_add_word_mode() {
    let c = coord("fromhist");
    push_commits(&c, &["你", "好"]);
    let mut st = c.state.lock().unwrap();
    c.open_add_word_from_history(&mut st);
    // 直开路径不得进入加词模式、不得改候选窗布局占位
    assert!(!st.add_word_active, "直开加词界面不应进入加词模式");
    assert!(st.add_word_chars.is_empty(), "不应填充加词字符池");
}

// ──────────────────────────────────────────────────────────────────────
// dict.add（`coad` 剪贴板加词的落点）
// ──────────────────────────────────────────────────────────────────────

#[test]
fn sanitize_trims_but_never_truncates() {
    use super::sanitize_dict_add_text;
    assert_eq!(sanitize_dict_add_text("  你好 \r\n").unwrap(), "你好");
    // 首尾换行属无损清理；内部换行则拒绝，不做一行化（宁可不加也不加错）。
    assert!(
        sanitize_dict_add_text("你好\n世界").is_err(),
        "内含换行应拒绝"
    );
    assert!(sanitize_dict_add_text("   \t \n ").is_err(), "空白串应拒绝");
    assert!(sanitize_dict_add_text("").is_err());
}

#[test]
fn derivable_word_rejects_non_han_and_overlong() {
    use super::{ADD_WORD_MAX_LEN, check_derivable_word};
    // 拼音（码来自读音）：非汉字一律拒绝，行为不变。
    assert!(check_derivable_word("你好", true).is_ok());
    assert!(
        check_derivable_word("hello", true).is_err(),
        "纯英文取不出码"
    );
    assert!(
        check_derivable_word("你好abc", true).is_err(),
        "混入非汉字应拒绝"
    );
    // 全角/中文标点是造词终止符、不是素材（见 is_han 的刻意排除）。
    assert!(
        check_derivable_word("你好，", true).is_err(),
        "中文标点应拒绝"
    );
    let long: String = std::iter::repeat_n('好', ADD_WORD_MAX_LEN + 1).collect();
    assert!(check_derivable_word(&long, true).is_err(), "超上限应拒绝");
    let ok: String = std::iter::repeat_n('好', ADD_WORD_MAX_LEN).collect();
    assert!(
        check_derivable_word(&ok, true).is_ok(),
        "恰好等于上限应放行"
    );
}

/// 码表方案只用中文部分取码，非汉字可以混在词里（姓名+手机号）。
#[test]
fn codetable_derivable_word_allows_non_han_around_han() {
    use super::{ADD_WORD_MAX_LEN, ADD_WORD_MAX_TEXT_LEN, check_derivable_word};
    assert!(check_derivable_word("张三13800138000", false).is_ok());
    assert!(check_derivable_word("你好，", false).is_ok());
    // 「有没有可取码的字」不在这里判（〇 这类有码的非汉字这里看不出来），交给取码；
    // 纯数字在取码那一步失败，见 `codetable_dict_add_derives_code_from_han_part`。
    assert!(check_derivable_word("二〇二六", false).is_ok());
    // 字数上限按汉字数算（它才是编码规则的上界）。
    let han: String = std::iter::repeat_n('好', ADD_WORD_MAX_LEN).collect();
    assert!(check_derivable_word(&format!("{han}123"), false).is_ok());
    let han: String = std::iter::repeat_n('好', ADD_WORD_MAX_LEN + 1).collect();
    assert!(check_derivable_word(&han, false).is_err());
    // 整段文本仍有总长上限：整句英文夹两个汉字不是「一个词」。
    let text = format!("你好{}", "a".repeat(ADD_WORD_MAX_TEXT_LEN - 1));
    assert!(check_derivable_word(&text, false).is_err());
}

/// 端到端：五笔下 `dict.add("张三13800138000")`（不给码）按「张三」的码入库。
#[test]
fn codetable_dict_add_derives_code_from_han_part() {
    let data = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../build_dev/data");
    if !data.join("schemas/wubi86.schema.toml").exists() {
        eprintln!("跳过：缺少 wubi86 schema");
        return;
    }
    let mut cfg = wind_config::config::Config::default();
    cfg.schema.available = vec!["wubi86".into()];
    cfg.schema.active = "wubi86".into();
    let db = std::env::temp_dir().join("wind_dictadd_gh171.redb");
    let _ = std::fs::remove_file(&db);
    let store = std::sync::Arc::new(wind_store::Store::open(&db).unwrap());
    let c = Coordinator::new_headless_with_store(cfg, Some(&data), store.clone());
    let han = c.engine_mgr.encode_word("wubi86", "张三").unwrap();
    c.cmd_dict_add("张三13800138000", "").unwrap();
    let schema = c.engine_mgr.data_schema_id("wubi86");
    let recs = store.get_user_words(&schema, &han).unwrap();
    // 〇 在五笔里有码，必须参与取码：入库码 = 「二〇二六年」整段的码，不是「二二六年」的。
    let year = c.engine_mgr.encode_word("wubi86", "二〇二六年").unwrap();
    assert_ne!(
        Ok(year.clone()),
        c.engine_mgr.encode_word("wubi86", "二二六年"),
        "〇 不该被跳过"
    );
    c.cmd_dict_add("二〇二六年", "").unwrap();
    let year_recs = store.get_user_words(&schema, &year).unwrap();
    // 一个可取码的字都没有 → 仍拒绝，且不写库。
    let err = c.cmd_dict_add("13800138000", "").unwrap_err().to_string();
    let _ = std::fs::remove_file(&db);
    assert!(
        recs.iter().any(|r| r.text == "张三13800138000"),
        "应按「张三」的码 {han} 入库：{recs:?}"
    );
    assert!(
        year_recs.iter().any(|r| r.text == "二〇二六年"),
        "应按含 〇 的码 {year} 入库：{year_recs:?}"
    );
    assert!(err.contains("取不出编码"), "{err}");
}

#[test]
fn toast_clamp_limits_length() {
    use super::toast_clamp;
    assert_eq!(toast_clamp("你好"), "你好");
    let long: String = std::iter::repeat_n('好', 40).collect();
    let out = toast_clamp(&long);
    assert_eq!(out.chars().count(), 17, "16 字 + 省略号");
    assert!(out.ends_with('…'));
}

/// 显式 code 路径：保持原行为（不受新增的汉字/长度守卫影响），照常写库。
#[test]
fn dict_add_with_explicit_code_writes_user_word() {
    let c = coord("dictadd_code");
    // 颜文字等无法自动取码的词条，显式给码时必须仍能加——新守卫只作用于推导路径。
    c.cmd_dict_add("(╯°□°)╯", "kaomoji").unwrap();
    let schema = c.engine_mgr.active_schema_id();
    let schema = c.engine_mgr.data_schema_id(&schema); // 与写入路径一致
    let store = c.store.as_ref().unwrap();
    let recs = store.get_user_words(&schema, "kaomoji").unwrap();
    assert_eq!(recs.len(), 1);
    assert_eq!(recs[0].text, "(╯°□°)╯");
}

/// 推导路径：headless 无引擎 → 取码为空 → 报错且不写库（不留空码孤儿记录）。
/// 这是 `coad` 的实际形态（`dict.add(clip())`，只有一个参数）。
#[test]
fn dict_add_without_code_takes_derive_path_and_reports() {
    let c = coord("dictadd_derive");
    let err = c.cmd_dict_add("你好", "").unwrap_err().to_string();
    // 关键：不再是「暂未支持自动推导编码」——已进入推导，只是 headless 取不出码。
    assert!(err.contains("取不出编码"), "应走推导路径并如实报因: {err}");
    // 隐私红线：错误消息会进 warn 日志，不得回显用户输入。
    assert!(!err.contains("你好"), "错误消息不得含词本身: {err}");
    let schema = c.engine_mgr.active_schema_id();
    let schema = c.engine_mgr.data_schema_id(&schema);
    let store = c.store.as_ref().unwrap();
    assert!(
        store.get_user_words(&schema, "").unwrap().is_empty(),
        "取码失败不得写入空码记录"
    );
}

/// 剪贴板整段文本（多行）走推导路径：拒绝并提示，不截断、不入库。
#[test]
fn dict_add_rejects_multiline_clipboard() {
    let c = coord("dictadd_multiline");
    let err = c
        .cmd_dict_add("第一行\n第二行", "")
        .unwrap_err()
        .to_string();
    assert!(err.contains("换行"), "应因换行被拒: {err}");
    assert!(!err.contains("第一行"), "错误消息不得含剪贴板内容: {err}");
}
