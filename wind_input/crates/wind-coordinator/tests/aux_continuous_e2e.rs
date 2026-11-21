//! 连续辅助码的协调器级端到端（真实数据 `build_dev/data`，缺数据跳过并打印原因）。
//!
//! 逐键 `handle_key_event`，读界面候选（引擎位次 ≠ 界面位次）、消费数与编码栏。配置矩阵：
//! 小鹤 / 自然码 / 微软双拼 × mini 语法模型开 / 关 × {`direct` 关（旧路径）、`direct` 开 + 逃生口
//! `continuous = false`（串尾直接辅助）、`direct` 开（连续辅助码）}。击键按布局由音节反编码，
//! 辅码取 `flypy_full.txt`（字形码与布局无关）：你 = rx、好 = nz、活 = dk。
//!
//! 夹具版（小词库、方案来源未就绪降级、组合中途改设置）在 `src/handle_direct_aux.rs` 的 tests。

use std::path::PathBuf;
use std::sync::Arc;
use wind_bridge::handler::{KeyAction, KeyEventData, MessageHandler};
use wind_config::Config;
use wind_coordinator::Coordinator;
use wind_engine::pinyin::shuangpin::{Layout, ShuangpinConverter};
use wind_ipc::protocol::{EVENT_KEY_DOWN, MOD_CTRL, MOD_SHIFT};
use wind_keys::keymap;
use wind_store::Store;

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../..")
}

fn data_dir() -> PathBuf {
    root().join("build_dev/data")
}

fn has_data() -> bool {
    let d = data_dir().join("schemas");
    let ok = d.join("shuangpin.schema.toml").is_file()
        && d.join("pinyin.schema.toml").is_file()
        && d.join("aux_code/flypy_full.txt").is_file()
        && d.join("pinyin/cn_dicts/base.dict.yaml").is_file()
        && d.join("pinyin/grammar/wanxiang-mini.gram").is_file();
    if !ok {
        eprintln!(
            "跳过：build_dev/data 缺双拼 / 全拼方案、flypy_full.txt、词库或语法模型（先跑 gen-data）"
        );
    }
    ok
}

const LAYOUTS: [&str; 3] = ["xiaohe", "ziranma", "mspy"];

#[derive(Clone, Copy, PartialEq, Debug)]
enum Mode {
    /// `direct` 关（旧路径）
    Old,
    /// `direct` 开 + 逃生口 `continuous = false`（串尾直接辅助）
    Tail,
    /// `direct` 开（连续辅助码）
    Cont,
}

struct TmpDir(PathBuf);

impl Drop for TmpDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

struct Env {
    c: Arc<Coordinator>,
    store: Arc<Store>,
    conv: ShuangpinConverter,
    name: String,
    _dir: TmpDir,
}

fn env(layout: &str, grammar: bool, mode: Mode, tweak: impl FnOnce(&mut Config)) -> Env {
    static N: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    let n = N.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let name = format!(
        "{layout} 模型{} {mode:?}",
        if grammar { "开" } else { "关" }
    );
    let dir = std::env::temp_dir().join(format!("wind_aux_e2e_{}_{n}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let ov = dir.join("ov");
    std::fs::create_dir_all(&ov).unwrap();
    let mut body = format!("[engine.pinyin.shuangpin]\nlayout = \"{layout}\"\n");
    if mode == Mode::Tail {
        body.push_str("[engine.aux_code]\ncontinuous = false\n");
    }
    std::fs::write(ov.join("shuangpin.toml"), body).unwrap();
    let mut cfg = Config::default();
    cfg.schema.available = vec!["shuangpin".into(), "pinyin".into()];
    cfg.schema.active = "shuangpin".into();
    cfg.input.default.chinese_mode = true;
    cfg.schema.pinyin.aux_code.enabled = true;
    cfg.schema.pinyin.aux_code.direct = mode != Mode::Old;
    cfg.schema.pinyin.frequency.enabled = true;
    if grammar {
        cfg.schema.pinyin.grammar.enabled = true;
        cfg.schema.pinyin.grammar.weight = 0.5;
        cfg.schema.pinyin.grammar.model = "wanxiang-mini.gram".to_string();
    }
    tweak(&mut cfg);
    let store = Arc::new(Store::open(dir.join("s.redb")).unwrap());
    let c = Coordinator::new_headless_with_store_override(
        cfg,
        Some(&data_dir()),
        store.clone(),
        Some(ov),
    );
    let conv = ShuangpinConverter::new(
        Layout::from_toml(&root().join(format!("data/schemas/shuangpin/{layout}.toml")))
            .expect("布局"),
    );
    Env {
        c,
        store,
        conv,
        name,
        _dir: TmpDir(dir),
    }
}

/// 三布局 × 模型开 / 关。
fn matrix(mode: Mode, mut each: impl FnMut(&Env)) {
    for layout in LAYOUTS {
        for grammar in [false, true] {
            each(&env(layout, grammar, mode, |_| {}));
        }
    }
}

/// 一键之后的界面状态：缓冲、编码栏、界面首选（`文字/消费数`，串尾一步提上来的加 `*`）。
#[derive(Debug, Clone, PartialEq)]
struct Snap {
    buf: String,
    preedit: String,
    top: String,
}

impl Env {
    /// 各音节的双拼击键拼起来。
    fn k(&self, syls: &[&str]) -> String {
        self.conv
            .build_reverse()
            .encode_all(syls)
            .expect("该布局可编码")
    }

    fn layout(&self) -> &str {
        self.name.split(' ').next().unwrap()
    }

    fn press_mod(&self, vk: u32, modifiers: u32) -> KeyAction {
        self.c.handle_key_event(&KeyEventData {
            key_code: vk,
            scan_code: 0,
            modifiers,
            event_type: EVENT_KEY_DOWN,
            toggles: 0,
            event_seq: 0,
            prev_char: 0,
            seq_char: 0,
        })
    }

    fn press(&self, vk: u32) -> KeyAction {
        self.press_mod(vk, 0)
    }

    fn key(&self, b: u8) -> KeyAction {
        let vk = match b {
            b';' => keymap::VK_SEMICOLON,
            b'\'' => keymap::VK_QUOTE,
            _ => keymap::VK_A + u32::from(b - b'a'),
        };
        self.press(vk)
    }

    fn snap(&self) -> Snap {
        let top = self
            .c
            .debug_candidate_consumed()
            .first()
            .map(|(t, n, d)| format!("{t}/{n}{}", if *d { "*" } else { "" }))
            .unwrap_or_default();
        Snap {
            buf: self.c.debug_input_buffer(),
            preedit: self.c.debug_preedit(),
            top,
        }
    }

    /// 清空后逐键敲入，返回每键之后的状态。
    fn type_each(&self, input: &str) -> Vec<Snap> {
        self.press(keymap::VK_ESCAPE);
        input
            .bytes()
            .map(|b| {
                self.key(b);
                self.snap()
            })
            .collect()
    }

    fn last(&self, input: &str) -> Snap {
        self.type_each(input).pop().unwrap()
    }

    /// 界面候选（`文字/消费数`）。
    fn cands(&self) -> Vec<String> {
        self.c
            .debug_candidate_consumed()
            .into_iter()
            .map(|(t, n, _)| format!("{t}/{n}"))
            .collect()
    }

    /// 选界面第 `idx` 条（逐条下移高亮再空格）。
    fn pick(&self, idx: usize) -> KeyAction {
        for _ in 0..idx {
            self.press(keymap::VK_DOWN);
        }
        self.press(keymap::VK_SPACE)
    }
}

fn committed(act: KeyAction) -> String {
    match act {
        KeyAction::InsertText { text, .. } => text,
        other => panic!("应上屏：{other:?}"),
    }
}

/// 编码栏去掉 `'` 与辅码分隔的空格后恰是缓冲（分隔符只按序插入，光标映射的前提）。
fn assert_preedit_is_buffer(e: &Env, s: &Snap, ctx: &str) {
    let bare: String = s
        .preedit
        .chars()
        .filter(|&c| c != '\'' && c != ' ')
        .collect();
    assert_eq!(bare, s.buf.replace('\'', ""), "{} {ctx}: {s:?}", e.name);
}

/// 不带辅码的形态：编码栏没有辅码分隔的空格、首选不是串尾一步提上来的。
fn assert_plain(e: &Env, s: &Snap, ctx: &str) {
    assert!(
        !s.preedit.contains(' ') && !s.top.ends_with('*'),
        "{} {ctx}: {s:?}",
        e.name
    );
}

fn snap_of(top: &str, preedit: &str, buf: &str) -> Snap {
    Snap {
        buf: buf.to_string(),
        preedit: preedit.to_string(),
        top: top.to_string(),
    }
}

/// 词中辅码 `ni` + `rx`（你）+ `hao`，逐键钉首选、消费数与编码栏。
/// 连续：第 3 键串尾一步（你/3*）、第 5 键「你和」、第 6 键「你好」消费整串、`ni rx'<hao>`；
/// 第 4 键小鹤 `rx` 是音节（ruan），辅码读法居首（你/4）；自然码 / 微软 `rx` 不成音节，首选
/// 「你让」只消费 3 键。串尾直接辅助与旧路径都不出「你好/6」；串尾只在单字 + 末尾辅码时提前。
#[test]
fn mid_aux_per_key() {
    if !has_data() {
        return;
    }
    matrix(Mode::Cont, |e| {
        let (ni, hao) = (e.k(&["ni"]), e.k(&["hao"]));
        let input = format!("{ni}rx{hao}");
        let v = e.type_each(&input);
        let h = &hao[..1];
        let k4 = if e.layout() == "xiaohe" {
            snap_of("你/4", "ni rx", "nirx")
        } else {
            snap_of("你让/3", "ni'r'x", "nirx")
        };
        let want = [
            snap_of("年/1", "n", "n"),
            snap_of("你/2", "ni", "ni"),
            snap_of("你/3*", "ni r", "nir"),
            k4,
            snap_of("你和/5", &format!("ni rx'{h}"), &input[..5]),
            snap_of("你好/6", &format!("ni rx'{hao}"), &input),
        ];
        assert_eq!(v, want, "{}", e.name);
        for s in &v {
            assert_preedit_is_buffer(e, s, &input);
        }
    });
    for mode in [Mode::Old, Mode::Tail] {
        matrix(mode, |e| {
            let input = format!("{}rx{}", e.k(&["ni"]), e.k(&["hao"]));
            let v = e.type_each(&input);
            // 串尾直接辅助在第 3 键（单字 1 位）提前；第 4 键（单字 2 位）也提前，小鹤除外：
            // `rx` 是音节 ruan，偶数长保全音节解析。
            let tail_keys: &[usize] = match (mode, e.layout()) {
                (Mode::Old, _) => &[],
                (_, "xiaohe") => &[2],
                _ => &[2, 3],
            };
            for (i, s) in v.iter().enumerate() {
                assert_preedit_is_buffer(e, s, &input);
                if !tail_keys.contains(&i) {
                    assert_plain(e, s, &input);
                }
            }
            assert_ne!(v[5].top, "你好/6", "{}", e.name);
            if mode == Mode::Tail {
                assert_eq!(v[2], snap_of("你/3*", "ni r", &input[..3]), "{}", e.name);
            }
            if tail_keys.contains(&3) {
                assert_eq!(v[3], snap_of("你/4*", "ni rx", &input[..4]), "{}", e.name);
            }
        });
    }
}

/// 串尾 `ni` + `hao` + `nz`（好）：连续模式第 5 键串尾一步（末字 1 位）、第 6 键引擎辅码整句
/// （`ni'<hao> nz`）；串尾直接辅助第 6 键不提前（偶数长保前 2 键）；旧路径都不带辅码。
#[test]
fn tail_aux_per_key() {
    if !has_data() {
        return;
    }
    for mode in [Mode::Old, Mode::Tail, Mode::Cont] {
        matrix(mode, |e| {
            let hao = e.k(&["hao"]);
            let input = format!("ni{hao}nz");
            let v = e.type_each(&input);
            assert_eq!(
                v[3],
                snap_of("你好/4", &format!("ni'{hao}"), &input[..4]),
                "{}",
                e.name
            );
            for s in &v {
                assert_preedit_is_buffer(e, s, &input);
            }
            match mode {
                Mode::Old => v.iter().for_each(|s| assert_plain(e, s, &input)),
                Mode::Tail => {
                    assert_eq!(
                        v[4],
                        snap_of("你好/5*", &format!("ni'{hao} n"), &input[..5]),
                        "{}",
                        e.name
                    );
                    assert_plain(e, &v[5], &input);
                    assert_ne!(v[5].top, "你好/6", "{}", e.name);
                }
                Mode::Cont => {
                    assert_eq!(
                        v[4..],
                        [
                            snap_of("你好/5*", &format!("ni'{hao} n"), &input[..5]),
                            snap_of("你好/6", &format!("ni'{hao} nz"), &input),
                        ],
                        "{}",
                        e.name
                    );
                }
            }
        });
    }
}

/// 设计 §9：辅码打在已对的字上不得破坏（`ug ho d vs de xn ui`，d 是「活」的辅码）：整串首选与
/// 不打辅码时同文、消费整串，编码栏 `<生活> d'<中…>`。旧路径同输入读不通（消费不到整串）。
#[test]
fn aux_on_correct_char_keeps_sentence() {
    if !has_data() {
        return;
    }
    matrix(Mode::Cont, |e| {
        let (head, tail) = (e.k(&["sheng", "huo"]), e.k(&["zhong", "de", "xiao", "shi"]));
        let plain = e.last(&format!("{head}{tail}"));
        let input = format!("{head}d{tail}");
        let v = e.type_each(&input);
        let s = v.last().unwrap();
        let (text, n) = s.top.rsplit_once('/').unwrap();
        assert_eq!(text, plain.top.rsplit_once('/').unwrap().0, "{}", e.name);
        assert_eq!(n, input.len().to_string(), "{}", e.name);
        assert!(
            s.preedit
                .starts_with(&format!("{}'{} d'", &head[..2], &head[2..])),
            "{}: {s:?}",
            e.name
        );
        for s in &v {
            assert_preedit_is_buffer(e, s, &input);
        }
    });
    matrix(Mode::Old, |e| {
        let input = format!(
            "{}d{}",
            e.k(&["sheng", "huo"]),
            e.k(&["zhong", "de", "xiao", "shi"])
        );
        let s = e.last(&input);
        assert_plain(e, &s, &input);
        assert!(
            !s.top.ends_with(&format!("/{}", input.len())),
            "{}: {s:?}",
            e.name
        );
    });
}

/// 奇数键闪动（设计 §6「须告知用户」）：`qi dai n` 一键之后再打一键即恢复「期待您」。
/// 不打辅码的逐键界面状态与串尾直接辅助（今天开着 `direct` 的用户）逐键相同。
#[test]
fn odd_key_flash_recovers() {
    if !has_data() {
        return;
    }
    for layout in LAYOUTS {
        for grammar in [false, true] {
            let cont = env(layout, grammar, Mode::Cont, |_| {});
            let tail = env(layout, grammar, Mode::Tail, |_| {});
            let input = cont.k(&["qi", "dai", "nin"]);
            let v = cont.type_each(&input);
            assert_eq!(v, tail.type_each(&input), "{}", cont.name);
            assert_eq!(
                v[5],
                snap_of(
                    "期待您/6",
                    &format!("qi'{}'{}", &input[2..4], &input[4..]),
                    &input
                ),
                "{}",
                cont.name
            );
            assert!(v[4].top.starts_with("期待"), "{}: {:?}", cont.name, v[4]);
        }
    }
}

/// 手打 `'`（`separator = "quote"`）：辅码不跨 `'`——`ni'rx…` 的 `rx` 是音节，不当「你」的辅码；
/// 辅码后的 `'` 并入辅码（`nirx'<hao>` → 你好，消费 7，编码栏 `ni rx'<hao>`）。
#[test]
fn quote_forces_syllable_split() {
    if !has_data() {
        return;
    }
    for layout in LAYOUTS {
        for grammar in [false, true] {
            let e = env(layout, grammar, Mode::Cont, |c| {
                c.schema.pinyin.separator = "quote".into();
            });
            let hao = e.k(&["hao"]);
            let input = format!("ni'rx{hao}");
            let v = e.type_each(&input);
            assert_eq!(v[2], snap_of("你/2", "ni'", "ni'"), "{}", e.name);
            for s in &v {
                assert_plain(&e, s, &input);
                assert_preedit_is_buffer(&e, s, &input);
            }
            assert!(
                !e.cands().iter().any(|c| c.starts_with("你好/")),
                "{}",
                e.name
            );
            let input = format!("nirx'{hao}");
            let v = e.type_each(&input);
            assert_eq!(
                v.last().unwrap(),
                &snap_of("你好/7", &format!("ni rx'{hao}"), &input),
                "{}",
                e.name
            );
        }
    }
}

/// 上屏：辅码整句整体上屏、缓冲为空；词频记在去辅码的 `nihao` 下，造的词（整句）记在
/// `shenghuozhongdexiaoshi` 下，带辅码字母的码一概不记。
#[test]
fn commit_clears_buffer_and_learns_plain_code() {
    if !has_data() {
        return;
    }
    for layout in LAYOUTS {
        for grammar in [false, true] {
            let e = env(layout, grammar, Mode::Cont, |c| {
                c.schema.pinyin.auto_learn.enabled = true;
            });
            e.type_each(&format!("nirx{}", e.k(&["hao"])));
            assert_eq!(committed(e.press(keymap::VK_SPACE)), "你好", "{}", e.name);
            assert_eq!(e.c.debug_input_buffer(), "", "{}", e.name);
            assert!(
                e.store
                    .get_freq("pinyin", "nihao", "你好")
                    .unwrap()
                    .is_some(),
                "{}",
                e.name
            );
            for code in ["nirxhao", "niruanhao", "nirhao"] {
                assert!(
                    e.store.get_freq("pinyin", code, "你好").unwrap().is_none(),
                    "{} {code}",
                    e.name
                );
            }
            let (head, tail) = (e.k(&["sheng", "huo"]), e.k(&["zhong", "de", "xiao", "shi"]));
            e.type_each(&format!("{head}d{tail}"));
            let text = committed(e.press(keymap::VK_SPACE));
            assert_eq!(text.chars().count(), 6, "{}: {text}", e.name);
            assert_eq!(e.c.debug_input_buffer(), "", "{}", e.name);
            let learned = e
                .store
                .get_temp_words("pinyin", "shenghuozhongdexiaoshi")
                .unwrap();
            assert!(
                learned.iter().any(|w| w.text == text),
                "{}: {learned:?}",
                e.name
            );
            for code in ["shenghuodzhongdexiaoshi", "shenghuodezhongdexiaoshi"] {
                assert!(
                    e.store.get_temp_words("pinyin", code).unwrap().is_empty(),
                    "{} {code}",
                    e.name
                );
            }
        }
    }
}

/// 分段上屏（待确认 #5 已定：不吞残留辅码）：选不带辅码的前缀候选后，紧随其后的辅码字母留在
/// 缓冲，按普通输入重解——候选、编码栏与单打剩余串相同。
#[test]
fn partial_commit_leaves_aux_letters() {
    if !has_data() {
        return;
    }
    matrix(Mode::Cont, |e| {
        let hao = e.k(&["hao"]);
        let (head, tail) = (e.k(&["sheng", "huo"]), e.k(&["zhong", "de", "xiao", "shi"]));
        for (input, pick, rest) in [
            (format!("nirx{hao}"), "你/2", format!("rx{hao}")),
            (format!("{head}d{tail}"), "生活/4", format!("d{tail}")),
        ] {
            let (fresh, fresh_cands) = (e.last(&rest), e.cands());
            e.type_each(&input);
            let idx = e
                .cands()
                .iter()
                .position(|c| c == pick)
                .unwrap_or_else(|| panic!("{}: 缺 {pick}：{:?}", e.name, e.cands()));
            // 已选的字留在组合区（编码栏前缀），剩余缓冲续打：缓冲、首选、候选同单打剩余串。
            // 剩余段的编码栏按两键一组显示（旧路径同形），不比单打时的分段。
            let word = pick.split('/').next().unwrap();
            let text = match e.pick(idx) {
                KeyAction::UpdateComposition { text, .. } => text,
                other => panic!("{}: 应分段上屏：{other:?}", e.name),
            };
            let s = e.snap();
            assert_eq!(text, s.preedit, "{}", e.name);
            let rest_shown = s
                .preedit
                .strip_prefix(word)
                .unwrap_or_else(|| panic!("{}: 编码栏应以已选的 {word} 开头：{s:?}", e.name));
            assert_preedit_is_buffer(e, &snap_of("", rest_shown, &s.buf), &input);
            assert!(!rest_shown.contains(' '), "{}: {s:?}", e.name);
            assert_eq!(
                (s.buf, s.top, e.cands()),
                (fresh.buf, fresh.top, fresh_cands),
                "{} {input}",
                e.name
            );
        }
    });
}

/// 回删：逐键退格回到的每一个前缀，界面状态与正向敲到该前缀时相同（快照作废后前缀兜底解码
/// 与连打取快照一致）。连续模式与串尾直接辅助各跑一遍。
#[test]
fn backspace_matches_typing() {
    if !has_data() {
        return;
    }
    for mode in [Mode::Tail, Mode::Cont] {
        matrix(mode, |e| {
            let hao = e.k(&["hao"]);
            let (head, tail) = (e.k(&["sheng", "huo"]), e.k(&["zhong", "de", "xiao", "shi"]));
            for input in [
                format!("nirx{hao}"),
                format!("ni{hao}nz"),
                format!("{head}d{tail}"),
            ] {
                let fwd = e.type_each(&input);
                for i in (0..fwd.len() - 1).rev() {
                    e.press(keymap::VK_BACK);
                    assert_eq!(e.snap(), fwd[i], "{} {input} 退到 {} 键", e.name, i + 1);
                }
            }
        });
    }
}

/// 不过闸的整串整句不抢界面首选（6.5e）：`wo men yi qi qu` + `chi` 的首键，「我们一期 + 辅码 + 是」
/// 与残码整句「我们一起去吃」同在消费整串一档、比分不过闸：界面首选仍是后者，编码栏不带辅码。
/// 模型关时「我们一期是」留在候选里（模型开时它不出）。
#[test]
fn failed_sn_not_ui_top() {
    if !has_data() {
        return;
    }
    matrix(Mode::Cont, |e| {
        let input = format!(
            "{}{}",
            e.k(&["wo", "men", "yi", "qi", "qu"]),
            &e.k(&["chi"])[..1]
        );
        let s = e.last(&input);
        let n = input.len();
        if !e.name.contains("模型开") {
            assert!(
                e.cands().contains(&format!("我们一期是/{n}")),
                "前提：不过闸的整句在候选里 {}：{:?}",
                e.name,
                e.cands()
            );
        }
        assert_eq!(s.top, format!("我们一起去吃/{n}"), "{}", e.name);
        assert_plain(e, &s, &input);
        assert_preedit_is_buffer(e, &s, &input);
    });
}

static TOGGLE_CFG: std::sync::Mutex<Option<Config>> = std::sync::Mutex::new(None);

fn toggle_overlay(cfg: &mut Config) {
    if let Some(c) = TOGGLE_CFG.lock().unwrap().clone() {
        *cfg = c;
    }
}

/// 热重载切 `direct`（设置页保存走这条）：关掉后逐键状态与 `direct` 关的新实例相同，再打开后与
/// 连续模式的新实例相同——重载前取的快照、建的辅码视图都不得跨开关沿用。
#[test]
fn direct_toggle_by_reload() {
    if !has_data() {
        return;
    }
    let cfg = |direct: bool| {
        let mut c = Config::default();
        c.schema.available = vec!["shuangpin".into(), "pinyin".into()];
        c.schema.active = "shuangpin".into();
        c.input.default.chinese_mode = true;
        c.schema.pinyin.aux_code.enabled = true;
        c.schema.pinyin.aux_code.direct = direct;
        c.schema.pinyin.frequency.enabled = true;
        c.normalize();
        c
    };
    let e = env("xiaohe", false, Mode::Cont, |c| *c = cfg(true));
    assert!(e.c.set_config_overlay(toggle_overlay));
    let (old, cont) = (
        env("xiaohe", false, Mode::Old, |_| {}),
        env("xiaohe", false, Mode::Cont, |_| {}),
    );
    let input = format!("nirx{}", e.k(&["hao"]));
    let want_cont = cont.type_each(&input);
    assert_eq!(want_cont[5].top, "你好/6", "前提：连续模式");
    assert_eq!(e.type_each(&input), want_cont);
    for (direct, want) in [(false, old.type_each(&input)), (true, want_cont)] {
        // 组合中途重载：缓冲里还留着前 4 键与它们的快照。
        e.type_each(&input[..4]);
        *TOGGLE_CFG.lock().unwrap() = Some(cfg(direct));
        // 返回 true 表示读盘失败。
        assert!(!e.c.reload_user_config(), "direct={direct}");
        assert_eq!(e.type_each(&input), want, "direct={direct}");
    }
}

/// 组合中途切走方案再切回：切回后连续模式照常，逐键状态与新开实例相同。
#[test]
fn schema_switch_round_trip() {
    if !has_data() {
        return;
    }
    for layout in LAYOUTS {
        let fresh = env(layout, false, Mode::Cont, |_| {});
        let e = env(layout, false, Mode::Cont, |c| {
            c.keys
                .key_actions
                .insert("ctrl+shift+p".into(), "switch_schema:pinyin".into());
            c.keys
                .key_actions
                .insert("ctrl+shift+s".into(), "switch_schema:shuangpin".into());
        });
        let input = format!("nirx{}", e.k(&["hao"]));
        e.type_each(&input[..4]);
        e.press_mod(keymap::VK_A + u32::from(b'p' - b'a'), MOD_CTRL | MOD_SHIFT);
        assert_eq!(e.c.debug_input_buffer(), "", "{}", e.name);
        e.type_each("nihao");
        assert_eq!(e.snap().top, "你好/5", "{}：全拼下", e.name);
        e.press(keymap::VK_ESCAPE);
        e.press_mod(keymap::VK_A + u32::from(b's' - b'a'), MOD_CTRL | MOD_SHIFT);
        assert_eq!(e.type_each(&input), fresh.type_each(&input), "{}", e.name);
        assert_eq!(e.snap().top, "你好/6", "{}", e.name);
    }
}
