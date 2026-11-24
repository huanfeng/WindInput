//! 小词库 + 小码表夹具的无头集成测试：真实的小鹤双拼引擎（布局取入库的
//! `data/schemas/shuangpin/xiaohe.toml`）、rime 源格式的迷你词库、`字=码` 码表，按键走
//! `handle_key_event` 入口。真实数据（`build_dev/data`）那组在下方 `real_data_tests`。
use crate::coordinator::Coordinator;
use crate::pipeline::ModeKind;
use std::sync::Arc;
use wind_bridge::handler::{KeyAction, KeyEventData, MessageHandler};
use wind_config::Config;
use wind_ipc::protocol::EVENT_KEY_DOWN;
use wind_keys::keymap;
use wind_store::Store;

/// 迷你词库。权重让「湿度」天然居首、「释读」垫底——直接辅助码要把它提上来。
const DICT: &str = "释读\tshi du\t10\n湿度\tshi du\t5000\n适度\tshi du\t3000\n\
                        十度\tshi du\t1000\n试读\tshi du\t500\n\
                        是\tshi\t9000\n十\tshi\t5000\n湿\tshi\t400\n释\tshi\t50\n\
                        读\tdu\t3000\n度\tdu\t2000\n\
                        国庆\tguo qing\t800\n国情\tguo qing\t600\n国\tguo\t5000\n\
                        想\txiang\t5000\n向\txiang\t4000\n像\txiang\t3000\n\
                        你好\tni hao\t3000\n拟好\tni hao\t200\n你号\tni hao\t100\n\
                        你\tni\t8000\n拟\tni\t300\n好\thao\t6000\n号\thao\t2000\n";
/// 小鹤形码（取自 flypy_full.txt；你 / 拟 / 好 / 号为夹具自拟）。
const AUX: &str = "释=pl\n湿=dy\n适=zk\n十=al\n试=yg\n读=yd\n度=gy\n是=or\n\
                       国=ky\n庆=gd\n情=xo\n想=mx\n向=pk\n像=rn\n\
                       你=rx\n拟=tw\n好=hn\n号=kd\n";

struct Fixture {
    dir: std::path::PathBuf,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// `aux` 是 `[engine.aux_code]` 里 `files` 之外的行（enabled / direct）；`scheme` 空 = 全拼。
fn fixture(tag: &str, scheme: &str, aux: &str) -> (std::path::PathBuf, Fixture) {
    fixture_with(tag, scheme, aux, "", "")
}

/// [`fixture`]，词库与码表各追加几行。
fn fixture_with(
    tag: &str,
    scheme: &str,
    aux: &str,
    more_dict: &str,
    more_aux: &str,
) -> (std::path::PathBuf, Fixture) {
    let dir = std::env::temp_dir().join(format!("wind_direct_aux_{tag}_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let schemas = dir.join("schemas");
    std::fs::create_dir_all(schemas.join("aux_code")).unwrap();
    std::fs::create_dir_all(schemas.join("sp")).unwrap();
    std::fs::create_dir_all(schemas.join("shuangpin")).unwrap();
    let layout = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../../data/schemas/shuangpin/xiaohe.toml");
    std::fs::copy(layout, schemas.join("shuangpin/xiaohe.toml")).unwrap();
    std::fs::write(
        schemas.join("sp/mini.dict.yaml"),
        format!("---\nname: mini\nversion: \"1\"\nsort: by_weight\n...\n{DICT}{more_dict}"),
    )
    .unwrap();
    std::fs::write(schemas.join("aux_code/t.txt"), format!("{AUX}{more_aux}")).unwrap();
    let pinyin = if scheme.is_empty() {
        String::new()
    } else {
        format!(
            "[engine.pinyin]\nscheme = \"{scheme}\"\n[engine.pinyin.shuangpin]\nlayout = \"xiaohe\"\n"
        )
    };
    std::fs::write(
        schemas.join("sp.schema.toml"),
        format!(
            "[schema]\nid = \"sp\"\nname = \"sp\"\n[engine]\ntype = \"pinyin\"\n{pinyin}\
                 [engine.aux_code]\nfiles = [\"aux_code/t.txt\"]\n{aux}\
                 [key_actions]\nbacktick = \"aux_code\"\n\
                 [[dictionaries]]\nid = \"mini\"\npath = \"sp/mini.dict.yaml\"\n\
                 type = \"rime_pinyin\"\ndefault = true\n"
        ),
    )
    .unwrap();
    (dir.clone(), Fixture { dir })
}

fn coord(tag: &str, data_dir: &std::path::Path) -> (Arc<Coordinator>, Arc<Store>) {
    coord_with(tag, data_dir, |_| {})
}

/// [`coord`]，另改一处配置。
fn coord_with(
    tag: &str,
    data_dir: &std::path::Path,
    tweak: impl FnOnce(&mut Config),
) -> (Arc<Coordinator>, Arc<Store>) {
    let path =
        std::env::temp_dir().join(format!("wind_direct_aux_{tag}_{}.redb", std::process::id()));
    let _ = std::fs::remove_file(&path);
    let store = Arc::new(Store::open(&path).unwrap());
    let mut cfg = Config::default();
    cfg.schema.available = vec!["sp".into()];
    cfg.schema.active = "sp".into();
    cfg.input.default.chinese_mode = true;
    // 出厂 L2 开着拼音调频，`Config::default()` 是类型默认（关）——不开就验不到记账码。
    cfg.schema.pinyin.frequency.enabled = true;
    tweak(&mut cfg);
    let c = Coordinator::new_headless_with_store(cfg, Some(data_dir), store.clone());
    (c, store)
}

fn press(c: &Coordinator, vk: u32) -> KeyAction {
    c.handle_key_event(&KeyEventData {
        key_code: vk,
        scan_code: 0,
        modifiers: 0,
        event_type: EVENT_KEY_DOWN,
        toggles: 0,
        event_seq: 0,
        prev_char: 0,
        seq_char: 0,
    })
}

fn type_str(c: &Coordinator, s: &str) {
    for ch in s.chars() {
        press(c, keymap::VK_A + (ch as u32 - 'a' as u32));
    }
}

fn texts(c: &Coordinator) -> Vec<String> {
    c.state
        .lock()
        .unwrap()
        .candidates
        .iter()
        .map(|c| c.text.clone())
        .collect()
}

const ON: &str = "enabled = true\ndirect = true\n";

/// 界面候选的（文字 / 消费数 / code）指纹。
fn fingerprint(c: &Coordinator) -> Vec<String> {
    c.state
        .lock()
        .unwrap()
        .candidates
        .iter()
        .map(|c| format!("{}/{}/{}", c.text, c.consumed_length, c.code))
        .collect()
}

/// 界面首选的（文字 / 消费数）。
fn top_tc(c: &Coordinator) -> Option<String> {
    c.state
        .lock()
        .unwrap()
        .candidates
        .first()
        .map(|c| format!("{}/{}", c.text, c.consumed_length))
}

/// 清空组合后敲入 `input`，返回界面候选指纹。
fn fp_after(c: &Coordinator, input: &str) -> Vec<String> {
    press(c, keymap::VK_ESCAPE);
    type_str(c, input);
    fingerprint(c)
}

/// 逃生口：`[engine.aux_code] continuous = false` 退回串尾直接辅助。下面钉「词组任一字」
/// （`uidu`+`p`、释 = pl）的老用例都跑在它下面：连续模式里那条规则已不做。
const TAIL_ONLY: &str = "enabled = true\ndirect = true\ncontinuous = false\n";

/// 连续模式不生效的三种配置（`direct` 关 / `enabled` 关 / 全拼）：界面候选（文字 / 消费数 /
/// code）与接入连续辅码之前逐字节相同。期望值是接入前用同一夹具录下的。
#[test]
fn continuous_off_is_byte_identical() {
    let sp_off = [
        (
            "nihchn",
            vec![
                "你好/4/nihao",
                "拟好/4/nihao",
                "你号/4/nihao",
                "你/2/ni",
                "拟/2/ni",
            ],
        ),
        (
            "uidup",
            vec![
                "湿度/4/shidu",
                "适度/4/shidu",
                "十度/4/shidu",
                "试读/4/shidu",
                "释读/4/shidu",
                "是/2/shi",
                "十/2/shi",
                "湿/2/shi",
                "释/2/shi",
            ],
        ),
        ("nitwhc", vec!["你/2/ni", "拟/2/ni"]),
        (
            "goqkg",
            vec![
                "国庆国/5/guoqingg",
                "国庆/4/guoqing",
                "国情/4/guoqing",
                "国/2/guo",
            ],
        ),
        ("xlrn", vec!["想/2/xiang", "向/2/xiang", "像/2/xiang"]),
    ];
    for (tag, aux) in [
        ("co_off_direct", "enabled = true\n"),
        ("co_off_total", "enabled = false\ndirect = true\n"),
    ] {
        let (dir, _f) = fixture(tag, "shuangpin", aux);
        let (c, _) = coord(tag, &dir);
        for (input, want) in &sp_off {
            assert_eq!(&fp_after(&c, input), want, "{tag} {input}");
        }
    }
    let (dir, _f) = fixture("co_quanpin", "", ON);
    let (c, _) = coord("co_quanpin", &dir);
    for (input, want) in [
        (
            "nihaohn",
            vec![
                "你好/5/nihao",
                "拟好/5/nihao",
                "你号/5/nihao",
                "你/2/ni",
                "拟/2/ni",
            ],
        ),
        (
            "shidup",
            vec![
                "湿度/5/shidu",
                "适度/5/shidu",
                "十度/5/shidu",
                "试读/5/shidu",
                "释读/5/shidu",
                "是/3/shi",
                "十/3/shi",
                "湿/3/shi",
                "释/3/shi",
            ],
        ),
        ("nitwhao", vec!["你/2/ni", "拟/2/ni"]),
    ] {
        assert_eq!(fp_after(&c, input), want, "quanpin {input}");
    }
}

/// 逃生口关掉连续模式：候选与接入前开着 `direct` 的串尾直接辅助逐字节相同（期望值同上，
/// 接入前录下）。
#[test]
fn continuous_escape_hatch_restores_tail() {
    let (dir, _f) = fixture("co_hatch", "shuangpin", TAIL_ONLY);
    let (c, _) = coord("co_hatch", &dir);
    for (input, want) in [
        (
            "nihchn",
            vec![
                "你好/6/nihao",
                "拟好/6/nihao",
                "你号/4/nihao",
                "你/2/ni",
                "拟/2/ni",
            ],
        ),
        (
            "uidup",
            vec![
                "释读/5/shidu",
                "湿度/4/shidu",
                "适度/4/shidu",
                "十度/4/shidu",
                "试读/4/shidu",
                "是/2/shi",
                "十/2/shi",
                "湿/2/shi",
                "释/2/shi",
            ],
        ),
        ("nitwhc", vec!["你/2/ni", "拟/2/ni"]),
        (
            "goqkk",
            vec!["国庆/5/guoqing", "国情/5/guoqing", "国/2/guo"],
        ),
        ("xlrn", vec!["像/4/xiang", "想/2/xiang", "向/2/xiang"]),
    ] {
        assert_eq!(fp_after(&c, input), want, "{input}");
    }
    assert!(c.continuous_aux_opts(&c.state.lock().unwrap()).is_none());
}

/// 连续模式：辅码打在词中（「拟」= tw，跟在 `ni` 后），界面首选即目标整句，消费整串；
/// 上屏后缓冲为空。反向对照：同输入在串尾直接辅助下首选不是它。
#[test]
fn continuous_mid_sentence_aux() {
    let (dir, _f) = fixture("co_mid", "shuangpin", ON);
    let (c, _) = coord("co_mid", &dir);
    assert!(
        c.continuous_aux_opts(&c.state.lock().unwrap()).is_some(),
        "direct 开 + 双拼 + 就绪：连续模式生效"
    );
    let fp = fp_after(&c, "nitwhc");
    assert_eq!(top_tc(&c).as_deref(), Some("拟好/6"), "{fp:?}");
    match press(&c, keymap::VK_SPACE) {
        KeyAction::InsertText { text, .. } => assert_eq!(text, "拟好"),
        other => panic!("应整体上屏：{other:?}"),
    }
    assert!(c.state.lock().unwrap().input_buffer.is_empty());
    let (dir, _f) = fixture("co_mid_tail", "shuangpin", TAIL_ONLY);
    let (c, _) = coord("co_mid_tail", &dir);
    type_str(&c, "nitwhc");
    assert_ne!(top_tc(&c).as_deref(), Some("拟好/6"));
}

/// 整句切换键（露 2 算 3）：过闸的整串整句（`nihhc`：你 + 辅码 h + 好）是池里第 1 名、界面首选
/// （它不带名次时会被末槽整句「你航 / 你行」压到第 3）；切换到末一格时换进来的那条照样消费
/// 整串，上屏后缓冲为空（辅码字母不滞留）。
#[test]
fn continuous_sn_in_sentence_pool() {
    let (dir, _f) = fixture_with(
        "co_pool",
        "shuangpin",
        ON,
        "行\thang\t5000\n藏\tcang\t3000\n才\tcai\t3000\n你航\tni hang\t3000\n",
        "你=hz\n",
    );
    let (c, _) = coord_with("co_pool", &dir, |cfg| {
        cfg.schema.pinyin.sentence_count = 2;
        cfg.schema.pinyin.sentence_max_count = 3;
        cfg.schema.pinyin.sentence_cycle_key = "tab".into();
    });
    type_str(&c, "nihhc");
    let pool: Vec<(String, usize)> = {
        let s = c.state.lock().unwrap();
        s.sentence_pool
            .iter()
            .map(|c| (c.text.clone(), c.consumed_length))
            .collect()
    };
    assert_eq!(pool.first(), Some(&("你好".to_string(), 5)), "{pool:?}");
    assert!(pool.len() >= 2, "前提：池里另有整句：{pool:?}");
    assert_eq!(top_tc(&c).as_deref(), Some("你好/5"));
    // 转一圈：每一步都有一条从池里换进来的「你好」，它消费整串。
    for _ in 1..pool.len() {
        press(&c, keymap::VK_TAB);
        assert_ne!(top_tc(&c).as_deref(), Some("你好/5"), "切换键换走了首位");
    }
    let at = c
        .state
        .lock()
        .unwrap()
        .candidates
        .iter()
        .position(|x| x.text == "你好")
        .expect("末一格窗口里池首那条在候选里");
    assert_eq!(fingerprint(&c)[at], "你好/5/nihao");
    match press(&c, keymap::VK_1 + at as u32) {
        KeyAction::InsertText { text, .. } => assert_eq!(text, "你好"),
        other => panic!("应整体上屏：{other:?}"),
    }
    assert!(c.state.lock().unwrap().input_buffer.is_empty());
}

/// 整串整句进池后，露 N 算 3（N = 1、2）：带名次的候选不多于 N（整句块与切换窗口都按它算），
/// 每按一次切换键窗口仍是 N 条，转满一圈回到「你好/5」居首。
#[test]
fn continuous_sn_pool_window_keeps_show_count() {
    let (dir, _f) = fixture_with(
        "co_pool_show",
        "shuangpin",
        ON,
        "行\thang\t5000\n藏\tcang\t3000\n才\tcai\t3000\n你航\tni hang\t3000\n",
        "你=hz\n",
    );
    for show in [1u8, 2] {
        let (c, _) = coord_with(&format!("co_pool_show{show}"), &dir, |cfg| {
            cfg.schema.pinyin.sentence_count = show;
            cfg.schema.pinyin.sentence_max_count = 3;
            cfg.schema.pinyin.sentence_cycle_key = "tab".into();
        });
        type_str(&c, "nihhc");
        let ranked = || -> Vec<(String, u8)> {
            let s = c.state.lock().unwrap();
            s.candidates
                .iter()
                .filter(|x| x.sentence_rank > 0)
                .map(|x| (x.text.clone(), x.sentence_rank))
                .collect()
        };
        let k = c.state.lock().unwrap().sentence_pool.len();
        assert!(k > show as usize, "前提：池比露的多（露 {show}）：{k}");
        assert_eq!(top_tc(&c).as_deref(), Some("你好/5"), "露 {show}");
        let r = ranked();
        assert!(r.len() <= show as usize, "露 {show}：{r:?}");
        for step in 1..=k {
            press(&c, keymap::VK_TAB);
            let r = ranked();
            assert!(r.len() <= show as usize, "露 {show} 第 {step} 次：{r:?}");
            if step < k {
                assert_ne!(
                    top_tc(&c).as_deref(),
                    Some("你好/5"),
                    "露 {show} 第 {step} 次"
                );
            }
        }
        assert_eq!(top_tc(&c).as_deref(), Some("你好/5"), "露 {show}：转满一圈");
    }
}

/// 连续模式整句候选的 code 去掉了辅码（`nitwhc` → `nihao`，不是双拼转换读出的 `niteihao`），
/// 词频随它记在 `nihao` 下。
#[test]
fn continuous_sentence_freq_goes_to_aux_free_code() {
    let (dir, _f) = fixture("co_freq", "shuangpin", ON);
    let (c, store) = coord("co_freq", &dir);
    type_str(&c, "nitwhc");
    assert_eq!(top_tc(&c).as_deref(), Some("拟好/6"));
    match press(&c, keymap::VK_SPACE) {
        KeyAction::InsertText { text, .. } => assert_eq!(text, "拟好"),
        other => panic!("应整体上屏：{other:?}"),
    }
    assert!(
        store.get_freq("pinyin", "nihao", "拟好").unwrap().is_some(),
        "应记在无辅码的 nihao 下"
    );
    assert!(
        store
            .get_freq("pinyin", "niteihao", "拟好")
            .unwrap()
            .is_none(),
        "带辅码读音的码不能进词频"
    );
}

/// 连续模式辅码候选的编码栏：音节之间 `'`、辅码前空格（`ni tw'hc`、串尾 `ni'hc hn`），
/// 界面候选的 code 是去辅码的拼音码；高亮移到不带辅码的候选时回到双拼分段。
#[test]
fn continuous_aux_preedit_shape() {
    let (dir, _f) = fixture("co_shape", "shuangpin", ON);
    let (c, _) = coord("co_shape", &dir);
    let fp = fp_after(&c, "nitwhc");
    assert_eq!(
        fp.first().map(String::as_str),
        Some("拟好/6/nihao"),
        "{fp:?}"
    );
    assert_eq!(c.debug_preedit(), "ni tw'hc");
    let fp = fp_after(&c, "nihchn");
    assert_eq!(
        fp.first().map(String::as_str),
        Some("你好/6/nihao"),
        "{fp:?}"
    );
    assert_eq!(c.debug_preedit(), "ni'hc hn");
    // 高亮移到第一条不带辅码的候选。
    let plain = {
        let st = c.state.lock().unwrap();
        st.candidates
            .iter()
            .position(|x| x.meta.aux_keys.is_none() && !x.is_direct_aux)
            .expect("应有不带辅码的候选")
    };
    for _ in 0..plain {
        press(&c, keymap::VK_DOWN);
    }
    assert_eq!(c.debug_preedit(), "ni'hc'hn");
}

/// 编码栏辅码分隔符走方案内部项 `preedit_separator`：引擎辅码整句与串尾直接辅助同一项；
/// 空串时辅码直接贴在音节后；白名单外的值（字母、`'`、`;`）不用、退回默认空格。
#[test]
fn continuous_preedit_separator_setting() {
    for (tag, line, mid, tail) in [
        (
            "co_sep_empty",
            "preedit_separator = \"\"\n",
            "nitw'hc",
            "ui'duy",
        ),
        (
            "co_sep_dash",
            "preedit_separator = \"-\"\n",
            "ni-tw'hc",
            "ui'du-y",
        ),
        (
            "co_sep_bad",
            "preedit_separator = \"x\"\n",
            "ni tw'hc",
            "ui'du y",
        ),
        (
            "co_sep_quote",
            "preedit_separator = \"'\"\n",
            "ni tw'hc",
            "ui'du y",
        ),
        (
            "co_sep_semicolon",
            "preedit_separator = \";\"\n",
            "ni tw'hc",
            "ui'du y",
        ),
    ] {
        let (dir, _f) = fixture(tag, "shuangpin", &format!("{ON}{line}"));
        let (c, _) = coord(tag, &dir);
        fp_after(&c, "nitwhc");
        assert_eq!(c.debug_preedit(), mid, "{tag}");
        let t = fp_after(&c, "uiduy");
        assert_eq!(t.first().map(String::as_str), Some("试读/5/shidu"), "{tag}");
        assert_eq!(c.debug_preedit(), tail, "{tag}");
    }
    // 逃生口下的串尾直接辅助同样走这一项。
    let (dir, _f) = fixture(
        "co_sep_tail",
        "shuangpin",
        &format!("{TAIL_ONLY}preedit_separator = \"\"\n"),
    );
    let (c, _) = coord("co_sep_tail", &dir);
    type_str(&c, "uidup");
    assert_eq!(c.debug_preedit(), "ui'dup");
}

/// 带辅码候选高亮时，光标在缓冲里逐位移动，编码栏里的插入符位置单调前进，且插入符之前的
/// 显示串去掉分隔符后恰是缓冲的那一段。
#[test]
fn aux_preedit_cursor_maps() {
    let (dir, _f) = fixture("co_caret", "shuangpin", ON);
    let (c, _) = coord("co_caret", &dir);
    type_str(&c, "nitwhc");
    let mut st = c.state.lock().unwrap();
    assert_eq!(st.preedit, "ni tw'hc");
    let mut last = None;
    for pos in 0..=st.input_buffer.len() {
        st.input_cursor_pos = pos;
        let d = c.ui_caret_bytes(&st);
        assert!(last.is_none_or(|l| d > l), "pos {pos}: {d} 不单调");
        last = Some(d);
        let shown: String = st.preedit[..d]
            .chars()
            .filter(|&ch| ch != '\'' && ch != ' ')
            .collect();
        assert_eq!(shown, st.input_buffer[..pos], "pos {pos}");
    }
}

/// 分段上屏（钉现状，待确认 #5）：`nirxhc` 首选「你好」把 `rx` 当「你」的辅码；改选不带辅码的
/// 单字「你」时不连带吞掉 `rx`，剩余缓冲 `rxhc` 照普通输入重解（候选同单打 `rxhc`）。
#[test]
fn partial_commit_leaves_aux_letters() {
    let (dir, _f) = fixture("co_partial", "shuangpin", ON);
    let (c, _) = coord("co_partial", &dir);
    let fresh = fp_after(&c, "rxhc");
    let fp = fp_after(&c, "nirxhc");
    assert_eq!(
        fp.first().map(String::as_str),
        Some("你好/6/nihao"),
        "{fp:?}"
    );
    let mut st = c.state.lock().unwrap();
    let (idx, ni) = st
        .candidates
        .iter()
        .enumerate()
        .find(|(_, x)| x.text == "你" && x.consumed_length == 2)
        .map(|(i, x)| (i, x.clone()))
        .expect("单字「你」");
    assert!(ni.meta.aux_keys.is_none());
    c.commit_selected(&mut st, &ni, idx as i32);
    assert_eq!(st.committed_text, "你");
    assert_eq!(st.input_buffer, "rxhc");
    let rest: Vec<String> = st
        .candidates
        .iter()
        .map(|x| format!("{}/{}/{}", x.text, x.consumed_length, x.code))
        .collect();
    assert_eq!(rest, fresh);
}

/// 候选调整按输入的归一码（`shadow_code`）记、按文字匹配，与候选 `code` 无关：对辅码整句
/// 置顶 / 让别的词置顶都照常生效，置顶后的辅码整句身份（消费数、码、编码栏）不变。
#[test]
fn continuous_shadow_pin_on_aux_sentence() {
    let (dir, _f) = fixture("co_shadow", "shuangpin", ON);
    let (c, store) = coord("co_shadow", &dir);
    fp_after(&c, "nitwhc");
    let (schema, key) = {
        let st = c.state.lock().unwrap();
        (c.engine_mgr.data_schema_id("sp"), st.shadow_code.clone())
    };
    assert!(!key.is_empty());
    store.pin_shadow(&schema, &key, "你", None, 0).unwrap();
    let fp = fp_after(&c, "nitwhc");
    assert_eq!(fp.first().map(String::as_str), Some("你/2/ni"), "{fp:?}");
    store.pin_shadow(&schema, &key, "拟好", None, 0).unwrap();
    let fp = fp_after(&c, "nitwhc");
    assert_eq!(
        fp.first().map(String::as_str),
        Some("拟好/6/nihao"),
        "{fp:?}"
    );
    assert_eq!(c.debug_preedit(), "ni tw'hc");
}

/// 上屏带辅码的合成整句后，自动造的词记在去辅码的拼音码下（`hcnirx` → 好你，记 `haoni`）。
#[test]
fn learn_code_has_no_aux_letters() {
    let (dir, _f) = fixture("co_learn", "shuangpin", ON);
    let (c, store) = coord_with("co_learn", &dir, |cfg| {
        cfg.schema.pinyin.auto_learn.enabled = true;
    });
    type_str(&c, "hcnirx");
    let top = {
        let st = c.state.lock().unwrap();
        st.candidates.first().cloned().expect("应有候选")
    };
    assert_eq!(
        (top.text.as_str(), top.consumed_length, top.code.as_str()),
        ("好你", 6, "haoni"),
        "前提：串尾辅码 rx 属「你」"
    );
    assert!(top.is_synthesized && top.meta.aux_keys.is_some());
    match press(&c, keymap::VK_SPACE) {
        KeyAction::InsertText { text, .. } => assert_eq!(text, "好你"),
        other => panic!("应整体上屏：{other:?}"),
    }
    let learned = store.get_temp_words("pinyin", "haoni").unwrap();
    assert!(learned.iter().any(|w| w.text == "好你"), "{learned:?}");
    for code in ["haoniruan", "haonirua", "haonir"] {
        assert!(
            store.get_temp_words("pinyin", code).unwrap().is_empty(),
            "{code}"
        );
    }
    assert!(store.get_freq("pinyin", "haoni", "好你").unwrap().is_some());
}

/// 连续模式下串尾一步只认末字（辅码限定紧挨在它前面的字，与万象 Pro 同）：末字 1 位
/// （`uidu`+`y`，读 = yd）、单字 1 位（`xl`+`r` → 像）照旧提前，编码栏照旧；「词组任一字」
/// （`uidu`+`p` 的释 = pl、`goqk`+`k` 的国 = ky）与单字 2 位（`xl`+`rn`）不提前（万象需 `/`）。
#[test]
fn tail_rules_in_continuous_are_last_char() {
    let (dir, _f) = fixture("co_tail", "shuangpin", ON);
    let (c, _) = coord("co_tail", &dir);
    let t = fp_after(&c, "uiduy");
    assert_eq!(&t[..2], ["试读/5/shidu", "释读/5/shidu"], "{t:?}");
    assert_eq!(c.debug_preedit(), "ui'du y");
    let t = fp_after(&c, "xlrn");
    assert!(
        c.state
            .lock()
            .unwrap()
            .candidates
            .iter()
            .all(|c| !c.is_direct_aux),
        "单字 2 位不提前：{t:?}"
    );
    assert_ne!(t.first().map(String::as_str), Some("像/4/xiang"), "{t:?}");
    let t = fp_after(&c, "xlr");
    assert_eq!(t.first().map(String::as_str), Some("像/3/xiang"), "{t:?}");
    let t = fp_after(&c, "goqkg");
    assert_eq!(
        t.first().map(String::as_str),
        Some("国庆/5/guoqing"),
        "{t:?}"
    );
    assert!(!t.iter().any(|x| x.starts_with("国情/5")), "{t:?}");
    // 任一字规则不再生效。
    let t = fp_after(&c, "uidup");
    assert!(!t.iter().any(|x| x == "释读/5/shidu"), "{t:?}");
    let t = fp_after(&c, "goqkk");
    assert!(
        !t.iter().any(|x| x.ends_with("/5/guoqing")),
        "国 = ky 是首字，不再提前：{t:?}"
    );
}

/// 连续模式下串尾命中的上屏与编码栏：整体上屏、组码清空，词频记前缀的拼音码（`shidu`）；
/// 高亮移到非命中项回到普通分段；单字模式下命中项照样过单字过滤。
#[test]
fn continuous_tail_hit_commit_preedit_single_char() {
    let (dir, _f) = fixture("co_commit", "shuangpin", ON);
    let (c, store) = coord("co_commit", &dir);
    type_str(&c, "uiduy");
    assert_eq!(c.debug_preedit(), "ui'du y");
    press(&c, keymap::VK_DOWN);
    press(&c, keymap::VK_DOWN);
    {
        let st = c.state.lock().unwrap();
        let hi = c.highlighted_global_index(&st);
        assert!(!st.candidates[hi].is_direct_aux, "第 3 位应是普通候选");
    }
    assert_eq!(c.debug_preedit(), "ui'du'y");
    press(&c, keymap::VK_ESCAPE);
    type_str(&c, "uiduy");
    match press(&c, keymap::VK_SPACE) {
        KeyAction::InsertText { text, .. } => assert_eq!(text, "试读"),
        other => panic!("应整体上屏：{other:?}"),
    }
    assert!(c.state.lock().unwrap().input_buffer.is_empty());
    assert!(store.get_freq("pinyin", "shidu", "试读").unwrap().is_some());
    assert!(
        store
            .get_freq("pinyin", "shiduy", "试读")
            .unwrap()
            .is_none()
    );
    type_str(&c, "xlr");
    c.set_single_char(true);
    let t = texts(&c);
    assert!(t.iter().all(|x| x.chars().count() == 1), "{t:?}");
    assert_eq!(t.first().map(String::as_str), Some("像"), "{t:?}");
}

/// 光标中间编辑与首单元读法另取整串的双拼分段（`convert(..).preedit_pinyin`），不传连续
/// 选项：分段来自双拼转换、与击键图无关，两种取法相同。
#[test]
fn continuous_preedit_split_same_as_plain_convert() {
    let (dir, _f) = fixture("co_split", "shuangpin", ON);
    let (c, _) = coord("co_split", &dir);
    type_str(&c, "n");
    let st = c.state.lock().unwrap();
    let lk = c.continuous_aux_lookup(&st).expect("连续模式生效").lookup;
    drop(st);
    for input in ["nitwhc", "uiduy", "xlrn", "goqkgd", "nitwh"] {
        let plain = c.engine_mgr.convert(input, 50).preedit_pinyin;
        let cont = c
            .engine_mgr
            .convert_active_with_opts(input, 50, super::continuous_opts(&lk, None))
            .preedit_pinyin;
        assert_eq!(cont, plain, "{input}");
    }
}

/// 引擎辅码整句与串尾命中同文同消费数时只出现一次（`nihchn`：好 = hn，你好 / 6）。
#[test]
fn tail_dedup_with_engine_sentence() {
    let (dir, _f) = fixture("co_dedup", "shuangpin", ON);
    let (c, _) = coord("co_dedup", &dir);
    let t = fp_after(&c, "nihchn");
    assert_eq!(t.first().map(String::as_str), Some("你好/6/nihao"), "{t:?}");
    assert_eq!(
        t.iter().filter(|x| x.starts_with("你好/")).count(),
        1,
        "{t:?}"
    );
}

/// 临拼 / 引导键辅助码态等非主输入路：不传连续辅码选项。
#[test]
fn continuous_respects_overlay() {
    let (dir, _f) = fixture("co_overlay", "shuangpin", ON);
    let (c, _) = coord("co_overlay", &dir);
    type_str(&c, "nitwhc");
    let mut st = c.state.lock().unwrap();
    assert!(c.continuous_aux_opts(&st).is_some(), "反向对照");
    for mode in [ModeKind::AuxCode, ModeKind::TempPinyin] {
        st.active = Some(mode);
        assert!(c.continuous_aux_opts(&st).is_none(), "{mode:?}");
    }
}

/// 翻页扩容只重跑 `build_candidates`：走同一份连续选项，首屏不变。
#[test]
fn continuous_expand_uses_same_opts() {
    let (dir, _f) = fixture("co_page", "shuangpin", ON);
    let (c, _) = coord("co_page", &dir);
    let before = fp_after(&c, "nitwhc");
    let mut st = c.state.lock().unwrap();
    st.has_more = true;
    c.expand_candidates(&mut st);
    drop(st);
    let after = fingerprint(&c);
    assert_eq!(after[..before.len().min(5)], before[..before.len().min(5)]);
    assert_eq!(top_tc(&c).as_deref(), Some("拟好/6"));
}

/// 组合中途改设置（设置页写方案覆盖层 → `aux_settings_gen` 推进）：关 `direct` / 开逃生口后
/// 下一键回旧路径，串尾快照不得跨模式复用；改回后恢复。
#[test]
fn continuous_invalidates_on_settings_change() {
    let (dir, _f) = fixture("co_inval", "shuangpin", ON);
    let ov = dir.join("overrides");
    std::fs::create_dir_all(&ov).unwrap();
    let path = std::env::temp_dir().join(format!(
        "wind_direct_aux_co_inval_{}.redb",
        std::process::id()
    ));
    let _ = std::fs::remove_file(&path);
    let store = Arc::new(Store::open(&path).unwrap());
    let mut cfg = Config::default();
    cfg.schema.available = vec!["sp".into()];
    cfg.schema.active = "sp".into();
    cfg.input.default.chinese_mode = true;
    let c = Coordinator::new_headless_with_store_override(cfg, Some(&dir), store, Some(ov));
    let write = |s: &str| {
        let v: toml::Value = s.parse().unwrap();
        c.engine_mgr.write_schema_override("sp", &v).unwrap();
    };
    let top = |c: &Coordinator| fingerprint(c).first().cloned();
    type_str(&c, "nitwhc");
    let continuous_snapshot = c
        .state
        .lock()
        .unwrap()
        .direct_aux_prev
        .as_ref()
        .map(|p| p.continuous);
    assert_eq!(continuous_snapshot, Some(true), "连续模式下取的快照");
    // 前缀 `uidu` 的快照在连续模式下取，切到逃生口后下一键不得复用（要按串尾模式重取）。
    press(&c, keymap::VK_ESCAPE);
    type_str(&c, "uidu");
    // 辅码设置 / 来源的代次也在复用判据里：改了设置，旧快照的代次与当下对不上。
    let snap_gen = c
        .state
        .lock()
        .unwrap()
        .direct_aux_prev
        .as_ref()
        .map(|p| p.data_gen);
    write("[engine.aux_code]\ncontinuous = true\n");
    assert_ne!(
        snap_gen,
        Some(c.direct_aux_data_gen()),
        "设置变了，快照代次作废"
    );
    write("[engine.aux_code]\ncontinuous = false\n");
    type_str(&c, "p");
    let prev = c
        .state
        .lock()
        .unwrap()
        .direct_aux_prev
        .as_ref()
        .map(|p| p.continuous);
    assert_eq!(prev, Some(false), "跨模式不复用快照");
    assert_eq!(top(&c).as_deref(), Some("释读/5/shidu"));
    write("[engine.aux_code]\ncontinuous = true\n");
    press(&c, keymap::VK_ESCAPE);
    type_str(&c, "nitwhc");
    for (ov, want_continuous) in [
        ("[engine.aux_code]\ncontinuous = false\n", false),
        ("[engine.aux_code]\ndirect = false\n", false),
        ("[engine.aux_code]\ncontinuous = true\n", true),
    ] {
        write(ov);
        press(&c, keymap::VK_BACK);
        type_str(&c, "c");
        assert_eq!(
            c.continuous_aux_opts(&c.state.lock().unwrap()).is_some(),
            want_continuous,
            "{ov}"
        );
        let got = top(&c);
        if want_continuous {
            assert_eq!(top_tc(&c).as_deref(), Some("拟好/6"), "{ov}");
        } else {
            assert_eq!(got.as_deref(), Some("你/2/ni"), "{ov}: 回旧路径");
        }
    }
}

/// 基线：同一夹具，不开直接辅助码时 `uidup` 的首选不是「释读」——否则下面的正向用例测不出东西。
#[test]
fn baseline_without_direct() {
    let (dir, _f) = fixture("base", "shuangpin", "enabled = true\n");
    let (c, _) = coord("base", &dir);
    type_str(&c, "uidu");
    assert_eq!(texts(&c).first().map(String::as_str), Some("湿度"));
    type_str(&c, "p");
    assert_ne!(texts(&c).first().map(String::as_str), Some("释读"));
}

/// 奇数长度、前缀 2 音节：命中项排最前；组码区显示「前缀 + 空格 + 辅码」；上屏连辅码一起
/// 吃掉、组码清空；调频记在前缀的拼音编码下（`shidu`），辅码字母不进词频。
#[test]
fn odd_hit_goes_first_and_commits_whole_buffer() {
    let (dir, _f) = fixture("odd", "shuangpin", TAIL_ONLY);
    let (c, store) = coord("odd", &dir);
    type_str(&c, "uidup");
    let t = texts(&c);
    assert_eq!(t.first().map(String::as_str), Some("释读"), "{t:?}");
    assert!(t.contains(&"湿度".to_string()), "其余候选去重接后：{t:?}");
    assert_eq!(t.iter().filter(|x| *x == "释读").count(), 1, "不重复");
    assert_eq!(
        c.debug_preedit(),
        "ui'du p",
        "高亮命中项：前缀音节 + 空格 + 辅码"
    );
    let act = press(&c, keymap::VK_SPACE);
    match act {
        KeyAction::InsertText { text, .. } => assert_eq!(text, "释读"),
        other => panic!("应整体上屏：{other:?}"),
    }
    let st = c.state.lock().unwrap();
    assert!(st.input_buffer.is_empty(), "上屏连辅码一起吃掉，组码清空");
    assert!(st.direct_aux_body.is_empty());
    drop(st);
    assert!(
        store.get_freq("pinyin", "shidu", "释读").unwrap().is_some(),
        "词频记在前缀的拼音编码下"
    );
    assert!(
        store
            .get_freq("pinyin", "shidup", "释读")
            .unwrap()
            .is_none()
    );
}

/// 连打时前缀取「几键前那一键」的候选快照；快照缺失（退格改了前缀、光标中间编辑…）
/// 时对前缀单独解码兜底。两条路给出同样的命中。
#[test]
fn prefix_snapshot_and_fallback_decode_agree() {
    let (dir, _f) = fixture("snap", "shuangpin", TAIL_ONLY);
    let (c, _) = coord("snap", &dir);
    type_str(&c, "uidu");
    {
        let st = c.state.lock().unwrap();
        let prev = st.direct_aux_prev.as_ref().expect("整音节输入应留下快照");
        assert_eq!(prev.input, "uidu");
    }
    type_str(&c, "p");
    let via_snapshot = texts(&c);
    // 奇数键不动快照，留给紧随其后的偶数键。
    assert_eq!(
        c.state
            .lock()
            .unwrap()
            .direct_aux_prev
            .as_ref()
            .map(|p| p.input.clone()),
        Some("uidu".into())
    );
    press(&c, keymap::VK_BACK);
    c.state.lock().unwrap().direct_aux_prev = None;
    type_str(&c, "p");
    assert_eq!(texts(&c), via_snapshot, "兜底解码与快照给出同样的候选");
    assert_eq!(via_snapshot.first().map(String::as_str), Some("释读"));
}

/// 输入不变而词库变了：候选右键删掉一个是命中项的用户词，原地重建（输入没变）时
/// 前缀快照还是删之前那份，不作废的话删掉的词照样顶在首位。
#[test]
fn deleting_user_word_hit_drops_prefix_snapshot() {
    let (dir, _f) = fixture("deluser", "shuangpin", TAIL_ONLY);
    let (c, store) = coord("deluser", &dir);
    // 释毒：用户词，释=pl 命中辅码 p；权重垫底，不开直接辅助时排不到前面。
    store
        .add_user_word("pinyin", "shidu", "释毒", 0, 0)
        .unwrap();
    type_str(&c, "uidup");
    let t = texts(&c);
    let idx = t
        .iter()
        .position(|x| x == "释毒")
        .expect("用户词应是命中项");
    assert!(
        c.state.lock().unwrap().candidates[idx].is_direct_aux,
        "{t:?}"
    );
    c.candidate_op(wind_ui_types::CandidateOp::Delete, idx);
    let t = texts(&c);
    assert!(
        !t.contains(&"释毒".to_string()),
        "删掉的用户词不得再出现：{t:?}"
    );
    assert_eq!(t.first().map(String::as_str), Some("释读"), "{t:?}");
}

/// 同上的通用一面：组合中途词库被别处写了（设置页加词），下一键不得复用旧快照。
#[test]
fn store_write_mid_composition_invalidates_snapshot() {
    let (dir, _f) = fixture("addmid", "shuangpin", TAIL_ONLY);
    let (c, store) = coord("addmid", &dir);
    type_str(&c, "uidu");
    store
        .add_user_word("pinyin", "shidu", "释毒", 0, 0)
        .unwrap();
    type_str(&c, "p");
    let t = texts(&c);
    let hit = c
        .state
        .lock()
        .unwrap()
        .candidates
        .iter()
        .any(|x| x.text == "释毒" && x.is_direct_aux);
    assert!(hit, "新加的用户词应是命中项：{t:?}");
}

/// 输入不变而切了单字模式：命中项同样要过单字过滤。快照只按前缀串认的话，会拿
/// 切换前那份（含词）直接复用，「释读」照样顶在单字模式的首位。
#[test]
fn single_char_toggle_filters_hits_too() {
    let (dir, _f) = fixture("single", "shuangpin", TAIL_ONLY);
    let (c, _) = coord("single", &dir);
    type_str(&c, "uidup");
    assert_eq!(texts(&c).first().map(String::as_str), Some("释读"));
    c.set_single_char(true);
    let t = texts(&c);
    assert!(
        t.iter().all(|x| x.chars().count() == 1),
        "单字模式下不得有词：{t:?}"
    );
    // 反向对照：单字模式下直接辅助照常工作（`uip` 前缀 shi，释 = pl 中 p）。
    press(&c, keymap::VK_ESCAPE);
    type_str(&c, "uip");
    let t = texts(&c);
    assert_eq!(
        t.first().map(String::as_str),
        Some("释"),
        "单字命中项照常提前：{t:?}"
    );
}

/// 分段上屏后缓冲换了一段，旧快照不再对应任何前缀，作废。
#[test]
fn partial_commit_drops_prefix_snapshot() {
    let (dir, _f) = fixture("partial", "shuangpin", ON);
    let (c, _) = coord("partial", &dir);
    // 奇数长度收尾：上屏后剩 `gok`，不是整音节输入，不会顺手覆盖快照。
    type_str(&c, "uidugok");
    let mut st = c.state.lock().unwrap();
    let shi = st
        .candidates
        .iter()
        .find(|c| c.text == "湿度")
        .cloned()
        .expect("湿度");
    assert_eq!(shi.consumed_length, 4, "分段候选");
    c.commit_selected(&mut st, &shi, 0);
    assert_eq!(st.input_buffer, "gok");
    let stale = st
        .direct_aux_prev
        .as_ref()
        .is_some_and(|p| !st.input_buffer.starts_with(&p.input));
    assert!(!stale, "分段上屏后不得留着上屏前的快照");
}

/// 高亮移到非命中项时组码区回到普通双拼分段。
#[test]
fn preedit_follows_highlight() {
    let (dir, _f) = fixture("hl", "shuangpin", TAIL_ONLY);
    let (c, _) = coord("hl", &dir);
    type_str(&c, "uidup");
    assert_eq!(c.debug_preedit(), "ui'du p");
    press(&c, keymap::VK_DOWN);
    let st = c.state.lock().unwrap();
    let hi = c.highlighted_global_index(&st);
    assert!(!st.candidates[hi].is_direct_aux, "第 2 位应是普通候选");
    drop(st);
    assert_eq!(c.debug_preedit(), "ui'du'p", "普通候选：原双拼分段");
}

/// 偶数长度：`xlrn` = xiang + `rn`，像=rn。逃生口（串尾）下无全音节候选可保留时命中项排最前；
/// 连续模式下单字 2 位不提前（万象需 `/` 才提前）。
#[test]
fn even_two_letter_aux() {
    let (dir, _f) = fixture("even", "shuangpin", TAIL_ONLY);
    let (c, _) = coord("even", &dir);
    type_str(&c, "xlrn");
    assert_eq!(
        texts(&c).first().map(String::as_str),
        Some("像"),
        "{:?}",
        texts(&c)
    );
    let (cdir, _f2) = fixture("even_c", "shuangpin", ON);
    let (cc, _) = coord("even_c", &cdir);
    type_str(&cc, "xlrn");
    assert!(
        cc.state
            .lock()
            .unwrap()
            .candidates
            .iter()
            .all(|c| !c.is_direct_aux),
        "连续模式：单字 2 位不提前：{:?}",
        texts(&cc)
    );
    // 国庆 / 国情：1 位辅码 k 两者都中（国=ky）；g 只中国庆（庆=gd，情=xo）。
    let (c, _) = coord("even2", &cdir);
    type_str(&c, "goqkg");
    let t = texts(&c);
    assert_eq!(t.first().map(String::as_str), Some("国庆"), "{t:?}");
    assert!(
        t.iter().position(|x| x == "国情") > Some(1),
        "国情不中 g，不被提前：{t:?}"
    );
}

/// 退格按原始按键串回滚：`uidup` 退一格回到 `uidu`，命中与组码区形态一并消失。
#[test]
fn backspace_rolls_back_to_plain_prefix() {
    let (dir, _f) = fixture("bs", "shuangpin", ON);
    let (c, _) = coord("bs", &dir);
    type_str(&c, "uidup");
    press(&c, keymap::VK_BACK);
    let st = c.state.lock().unwrap();
    assert_eq!(st.input_buffer, "uidu");
    assert!(st.direct_aux_body.is_empty());
    assert!(st.candidates.iter().all(|c| !c.is_direct_aux));
    assert_eq!(st.candidates.first().map(|c| c.text.as_str()), Some("湿度"));
}

/// 翻页扩容只重跑 `build_candidates`：命中项必须还在（插入点放在它里面的理由）。
#[test]
fn hits_survive_page_expansion() {
    let (dir, _f) = fixture("page", "shuangpin", TAIL_ONLY);
    let (c, _) = coord("page", &dir);
    type_str(&c, "uidup");
    let mut st = c.state.lock().unwrap();
    st.has_more = true;
    c.expand_candidates(&mut st);
    assert_eq!(st.candidates.first().map(|c| c.text.as_str()), Some("释读"));
    assert!(st.candidates[0].is_direct_aux);
}

/// 四种不生效：`direct` 关 / `enabled` 关 / 全拼 / 引导键态中。
#[test]
fn not_applicable_cases_leave_candidates_alone() {
    for (tag, scheme, aux) in [
        ("off_direct", "shuangpin", "enabled = true\n"),
        ("off_total", "shuangpin", "enabled = false\ndirect = true\n"),
        ("quanpin", "", ON),
    ] {
        let (dir, _f) = fixture(tag, scheme, aux);
        let (c, _) = coord(tag, &dir);
        type_str(&c, if scheme.is_empty() { "shidup" } else { "uidup" });
        let st = c.state.lock().unwrap();
        assert!(
            st.candidates.iter().all(|c| !c.is_direct_aux),
            "{tag}: 不应有直接辅助命中"
        );
        assert!(st.direct_aux_body.is_empty(), "{tag}");
    }
    // 引导键态：直接辅助停用（同一输入，门卫先看 `state.active`）。
    let (dir, _f) = fixture("guide", "shuangpin", TAIL_ONLY);
    let (c, _) = coord("guide", &dir);
    type_str(&c, "uidup");
    let mut st = c.state.lock().unwrap();
    st.active = Some(ModeKind::AuxCode);
    let mut cands = vec![wind_candidate::Candidate {
        text: "湿度".into(),
        ..Default::default()
    }];
    c.apply_direct_aux(&mut st, &mut cands, 50, false, None);
    assert_eq!(cands.len(), 1);
    assert!(st.direct_aux_body.is_empty());
    st.active = None;
    drop(st);
    // 反向对照：同一 coordinator 在主输入路上确实会命中（上面那条不是因为别的原因空转）。
    assert_eq!(texts(&c).first().map(String::as_str), Some("释读"));
}

/// 方案来源（`schema:<id>`）的系统层没就绪：本键不做直接辅助、候选原样，派后台构建；
/// 建好后下一键即生效。按键线程绝不现建反查索引（与引导键进入同一处理）。
#[test]
fn schema_source_not_ready_is_noop_then_warms() {
    let id = format!("zz_da_cold_{}", std::process::id());
    struct CacheGuard(String);
    impl Drop for CacheGuard {
        fn drop(&mut self) {
            if let Some(cache) = Config::cache_dir() {
                let _ = std::fs::remove_dir_all(cache.join(&self.0));
            }
        }
    }
    let _cg = CacheGuard(id.clone());
    let (dir, _f) = fixture("cold", "shuangpin", TAIL_ONLY);
    let schemas = dir.join("schemas");
    std::fs::create_dir_all(schemas.join(&id)).unwrap();
    std::fs::write(
        schemas.join(format!("{id}.schema.toml")),
        format!(
            "[schema]\nid = \"{id}\"\nname = \"形\"\n[engine]\ntype = \"codetable\"\n\
                 [[dictionaries]]\nid = \"{id}_main\"\npath = \"{id}/{id}.dict.yaml\"\n\
                 type = \"rime_codetable\"\ndefault = true\n"
        ),
    )
    .unwrap();
    std::fs::write(
        schemas.join(format!("{id}/{id}.dict.yaml")),
        format!(
            "---\nname: {id}\nversion: \"1\"\ncolumns:\n  - code\n  - text\n  - weight\n...\n\
                 pl\t释\t10\ndy\t湿\t10\nyd\t读\t10\ngy\t度\t10\n"
        ),
    )
    .unwrap();
    let sp = schemas.join("sp.schema.toml");
    let text = std::fs::read_to_string(&sp).unwrap();
    let swapped = text.replace(r#"["aux_code/t.txt"]"#, &format!(r#"["schema:{id}"]"#));
    assert_ne!(swapped, text);
    std::fs::write(&sp, swapped).unwrap();
    let (c, _) = coord("cold", &dir);
    type_str(&c, "uidup");
    // 「未就绪 ⇒ 原样」只在后台索引此刻确实没建好时才能断言：词库很小，并行跑测试时后台线程
    // 可能抢在第一键前建完，按「未就绪」断言就是假红。
    if c.engine_mgr.reverse_index_if_ready(&id).is_none() {
        let st = c.state.lock().unwrap();
        assert!(
            st.candidates.iter().all(|c| !c.is_direct_aux),
            "未就绪：原样"
        );
        assert_ne!(st.candidates.first().map(|c| c.text.as_str()), Some("释读"));
    } else {
        eprintln!("后台索引已先于首键建好，跳过「未就绪」断言，只验就绪后生效");
    }
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while c.engine_mgr.reverse_index_if_ready(&id).is_none() {
        assert!(
            std::time::Instant::now() < deadline,
            "后台构建 5 秒内没建好"
        );
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    press(&c, keymap::VK_BACK);
    type_str(&c, "p");
    assert_eq!(
        texts(&c).first().map(String::as_str),
        Some("释读"),
        "就绪后下一键即生效"
    );
}

/// 方案来源的反查索引未就绪：本键整条回旧路径（不传击键图、不做串尾），候选与
/// `direct` 关时相同，并派后台构建；建好后下一键走连续路径。
#[test]
fn continuous_unready_falls_back() {
    let id = format!("zz_co_cold_{}", std::process::id());
    struct CacheGuard(String);
    impl Drop for CacheGuard {
        fn drop(&mut self) {
            if let Some(cache) = Config::cache_dir() {
                let _ = std::fs::remove_dir_all(cache.join(&self.0));
            }
        }
    }
    let _cg = CacheGuard(id.clone());
    let (dir, _f) = fixture("co_cold", "shuangpin", ON);
    let schemas = dir.join("schemas");
    std::fs::create_dir_all(schemas.join(&id)).unwrap();
    std::fs::write(
        schemas.join(format!("{id}.schema.toml")),
        format!(
            "[schema]\nid = \"{id}\"\nname = \"形\"\n[engine]\ntype = \"codetable\"\n\
                 [[dictionaries]]\nid = \"{id}_main\"\npath = \"{id}/{id}.dict.yaml\"\n\
                 type = \"rime_codetable\"\ndefault = true\n"
        ),
    )
    .unwrap();
    std::fs::write(
        schemas.join(format!("{id}/{id}.dict.yaml")),
        format!(
            "---\nname: {id}\nversion: \"1\"\ncolumns:\n  - code\n  - text\n  - weight\n...\n\
                 rx\t你\t10\ntw\t拟\t10\nhn\t好\t10\nkd\t号\t10\n"
        ),
    )
    .unwrap();
    let sp = schemas.join("sp.schema.toml");
    let text = std::fs::read_to_string(&sp).unwrap();
    let swapped = text.replace(r#"["aux_code/t.txt"]"#, &format!(r#"["schema:{id}"]"#));
    assert_ne!(swapped, text);
    std::fs::write(&sp, swapped).unwrap();
    let (c, _) = coord("co_cold", &dir);
    // 放弃标记钉死「未就绪」：不起后台构建，不靠时序。
    c.engine_mgr.mark_reverse_index_skipped_for_test(&id, true);
    type_str(&c, "nitwhc");
    assert!(c.engine_mgr.reverse_index_if_ready(&id).is_none());
    assert!(
        c.continuous_aux_opts(&c.state.lock().unwrap()).is_none(),
        "未就绪：不传连续选项（不是「击键图开、无辅码」）"
    );
    assert_eq!(
        fingerprint(&c),
        ["你/2/ni", "拟/2/ni"],
        "未就绪：同 direct 关"
    );
    assert!(
        c.state
            .lock()
            .unwrap()
            .candidates
            .iter()
            .all(|c| !c.is_direct_aux)
    );
    // 解除标记、同步建好索引，下一键起走连续路径。
    c.engine_mgr.mark_reverse_index_skipped_for_test(&id, false);
    assert!(c.engine_mgr.prewarm_reverse_index(&id));
    press(&c, keymap::VK_BACK);
    type_str(&c, "c");
    assert!(c.continuous_aux_opts(&c.state.lock().unwrap()).is_some());
    assert_eq!(
        top_tc(&c).as_deref(),
        Some("拟好/6"),
        "就绪后下一键走连续路径"
    );
}

/// 连续模式下连打（取几键前的快照）与退格重打（快照作废、前缀兜底解码）给出同样的候选。
#[test]
fn continuous_snapshot_and_fallback_agree() {
    let (dir, _f) = fixture("co_snap", "shuangpin", ON);
    let (c, _) = coord("co_snap", &dir);
    for input in ["uiduy", "xlrn", "goqkg", "nihchn"] {
        let through = fp_after(&c, input);
        press(&c, keymap::VK_BACK);
        c.state.lock().unwrap().direct_aux_prev = None;
        type_str(&c, &input[input.len() - 1..]);
        assert_eq!(fingerprint(&c), through, "{input}");
    }
}

/// 直接辅助的结果上仍可按反引号进引导键筛选，筛的是当前整张候选表。
#[test]
fn guide_key_filters_current_list() {
    let (dir, _f) = fixture("guide2", "shuangpin", TAIL_ONLY);
    let (c, _) = coord("guide2", &dir);
    type_str(&c, "uidup");
    press(&c, keymap::VK_BACKTICK);
    let st = c.state.lock().unwrap();
    assert_eq!(st.active, Some(ModeKind::AuxCode));
    assert_eq!(st.candidates.first().map(|c| c.text.as_str()), Some("释读"));
}
