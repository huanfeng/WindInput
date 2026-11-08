//! 系统词模糊命中不依赖「猜中」的那条切分。
//!
//! `pinan` 在音节图上有 `pi|nan` 与 `pin|an` 两条等长切分。模糊变体若只按其中一条逐音节展开，
//! 另一条上的 `pin|an → ping|an`（平安）就永远查不到，用户只能打 `pin'an`。本文件钉的是
//! 「每条切分上的变体都要查」，并附「模糊音关着不出」的对照，证明命中来自模糊路径。
//!
//! 每条用例还钉住开 / 关模糊音两种情形下的首选与编码栏：编码栏跟随首选，模糊命中的首选
//! 要按它实际走的那条**所打码**切分显示（`pin'an`），而不是 `maximum_match` 的 `pi'nan`。
//!
//! 位次在 **Coordinator 层**取（协调器会按消费长度等再排一次，引擎位次 ≠ 界面位次）。
//!
//! ⚠️ 词典缺失时整族静默跳过（判据是**耗时 0.00s**，不是通过条数）。

use std::path::PathBuf;
use std::sync::Arc;
use wind_bridge::handler::{KeyEventData, MessageHandler};
use wind_config::Config;
use wind_coordinator::Coordinator;
use wind_ipc::protocol::EVENT_KEY_DOWN;
use wind_store::Store;

fn data_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../build_dev/data")
}

fn has_pinyin() -> bool {
    data_dir()
        .join("schemas/pinyin/cn_dicts/base.dict.yaml")
        .exists()
}

fn candidates_for(input: &str, apply: impl FnOnce(&mut Config)) -> Vec<String> {
    typed(input, apply).0
}

/// 敲完 `input` 后的（界面候选, 编码栏）。
fn typed(input: &str, apply: impl FnOnce(&mut Config)) -> (Vec<String>, String) {
    let mut cfg = Config::default();
    cfg.schema.available = vec!["pinyin".into()];
    cfg.schema.active = "pinyin".into();
    cfg.input.default.chinese_mode = true;
    apply(&mut cfg);
    let coord = Coordinator::new_headless(cfg, Some(&data_dir()));
    type_keys(&coord, input);
    (coord.debug_all_candidate_texts(), coord.debug_preedit())
}

fn type_keys(coord: &Coordinator, input: &str) {
    for c in input.chars() {
        coord.handle_key_event(&KeyEventData {
            key_code: (c.to_ascii_uppercase() as u32) & 0xFF,
            scan_code: 0,
            modifiers: 0,
            event_type: EVENT_KEY_DOWN,
            toggles: 0,
            event_seq: 0,
            prev_char: 0,
            seq_char: 0,
        });
    }
}

/// 开关打开时 `input` 的界面候选前 `top` 项含 `want`，模糊音关着时全列表不含。
fn assert_fuzzy_recalls(input: &str, want: &str, top: usize, on: fn(&mut Config)) {
    let with = candidates_for(input, |cfg| {
        cfg.schema.pinyin.fuzzy.enabled = true;
        on(cfg);
    });
    let head = &with[..with.len().min(top)];
    assert!(
        head.iter().any(|t| t == want),
        "开模糊音后 {input} 前 {top} 项须有「{want}」，实际: {head:?}"
    );
    let without = candidates_for(input, |_| {});
    assert!(
        !without.iter().any(|t| t == want),
        "模糊音关着时 {input} 不该出「{want}」，否则本测试证明不了模糊路径生效"
    );
}

/// 编码栏跟随首选：开模糊音（`on` 打开的那组）与关模糊音时，各自的（首选, 编码栏）。
///
/// 关模糊音那份是对照：证明编码栏的变化来自模糊路径，且关着时原样不动。
fn assert_top_preedit(input: &str, on: fn(&mut Config), fuzzy: (&str, &str), plain: (&str, &str)) {
    let check =
        |label: &str, (cands, preedit): (Vec<String>, String), (top, want): (&str, &str)| {
            assert_eq!(
                cands.first().map(String::as_str),
                Some(top),
                "前提：{label}时 {input} 首选应是「{top}」，实际: {:?}",
                &cands[..cands.len().min(5)]
            );
            assert_eq!(preedit, want, "{label}时 {input} 首选「{top}」的编码栏");
        };
    let with = typed(input, |cfg| {
        cfg.schema.pinyin.fuzzy.enabled = true;
        on(cfg);
    });
    check("开模糊音", with, fuzzy);
    check("关模糊音", typed(input, |_| {}), plain);
}

fn in_ing(cfg: &mut Config) {
    cfg.schema.pinyin.fuzzy.in_ing = true;
}

fn an_ang(cfg: &mut Config) {
    cfg.schema.pinyin.fuzzy.an_ang = true;
}

/// 报障原例：`pi|nan` 与 `pin|an` 等长，平安在后一条切分上。
/// 关模糊音时首选是整句「拼按」，同样走 `pin|an`。
#[test]
fn in_ing_pinan_recalls_pingan() {
    if !has_pinyin() {
        return;
    }
    assert_fuzzy_recalls("pinan", "平安", 5, in_ing);
    assert_top_preedit("pinan", in_ing, ("平安", "pin'an"), ("拼按", "pin'an"));
}

/// 多切分展开后，变体码会撞上切法不同的词：只开 an↔ang 时 `xi|an → xi|ang` 拼出 `xiang`，
/// 而「想」是单音节 `xiang`，不是 `xi|ang`。词条边界与变体边界同域，对不上就不能进图，
/// 否则 `kandaoxian` 的整句成了「看到想」（`xian → xiang` 要开 ian↔iang 才算）。
#[test]
fn an_ang_does_not_borrow_words_across_segmentations() {
    if !has_pinyin() {
        return;
    }
    let got = candidates_for("kandaoxian", |cfg| {
        cfg.schema.pinyin.fuzzy.enabled = true;
        an_ang(cfg);
    });
    let head = &got[..got.len().min(5)];
    assert!(
        !head.iter().any(|t| t.contains('想')),
        "只开 an↔ang 时 kandaoxian 前 5 项不该出现「想」，实际: {head:?}"
    );
    let want = ("看导线", "kan'dao'xian");
    assert_top_preedit("kandaoxian", an_ang, want, want);
}

/// 韵母组换成 an↔ang：`fa|nan`（发难）与 `fan|an → fang|an`（方案）。
/// 首选是精确词「翻案」（`fan|an`），开关模糊音都不变。
#[test]
fn an_ang_fanan_recalls_fangan() {
    if !has_pinyin() {
        return;
    }
    assert_fuzzy_recalls("fanan", "方案", 5, an_ang);
    let want = ("翻案", "fan'an");
    assert_top_preedit("fanan", an_ang, want, want);
}

/// 整串已有精确词时（`ji|nan` 济南），平安那类整句兜不住：`jin|an → jing|an`（静安）只能靠
/// 词候选那一路查出来，而它此前只按 `maximum_match` 那一条切分展开变体。
/// 首选仍是济南，编码栏跟随它显示 `ji'nan`。
#[test]
fn in_ing_jinan_recalls_jingan() {
    if !has_pinyin() {
        return;
    }
    assert_fuzzy_recalls("jinan", "静安", 9, in_ing);
    let want = ("济南", "ji'nan");
    assert_top_preedit("jinan", in_ing, want, want);
}

/// 同上，另一组真实竞争：`xi|nan`（西南）占着整串，`xin|an → xing|an`（兴安）只能靠词候选。
#[test]
fn in_ing_xinan_recalls_xingan() {
    if !has_pinyin() {
        return;
    }
    assert_fuzzy_recalls("xinan", "兴安", 9, in_ing);
    let want = ("西南", "xi'nan");
    assert_top_preedit("xinan", in_ing, want, want);
}

/// 前缀子码同口径：`jinanshi` 的前缀 `jinan` 也要按全部切分展开（静安）。
#[test]
fn in_ing_prefix_jinanshi_recalls_jingan() {
    if !has_pinyin() {
        return;
    }
    assert_fuzzy_recalls("jinanshi", "静安", 20, in_ing);
    let want = ("济南市", "ji'nan'shi");
    assert_top_preedit("jinanshi", in_ing, want, want);
}

/// 词候选那一路同样核对边界：只开 an↔ang 时 `xian` 的 `xi|an → xi|ang` 不能借来单音节的「想」。
#[test]
fn an_ang_xian_word_candidates_do_not_borrow_xiang() {
    if !has_pinyin() {
        return;
    }
    let got = candidates_for("xian", |cfg| {
        cfg.schema.pinyin.fuzzy.enabled = true;
        an_ang(cfg);
    });
    let head = &got[..got.len().min(30)];
    assert!(
        !head.iter().any(|t| t == "想"),
        "只开 an↔ang 时 xian 前 30 项不该有「想」，实际: {head:?}"
    );
    let want = ("先", "xian");
    assert_top_preedit("xian", an_ang, want, want);
}

/// 另一对切分：`di|nan`（堤南）与 `din|an → ding|an`（定安）。
#[test]
fn in_ing_dinan_preedit_follows_dingan() {
    if !has_pinyin() {
        return;
    }
    assert_top_preedit("dinan", in_ing, ("定安", "din'an"), ("堤南", "di'nan"));
}

/// 韵母组换成 an↔ang：`cha|nan` 与 `chan|an → chang|an`（长安）。
#[test]
fn an_ang_chanan_preedit_follows_changan() {
    if !has_pinyin() {
        return;
    }
    assert_top_preedit("chanan", an_ang, ("长安", "chan'an"), ("差难", "cha'nan"));
}

/// 三音节整词同口径（`chan|an|jie → chang|an|jie` 长安街）。
#[test]
fn an_ang_chananjie_preedit_follows_changanjie() {
    if !has_pinyin() {
        return;
    }
    assert_top_preedit(
        "chananjie",
        an_ang,
        ("长安街", "chan'an'jie"),
        ("差难解", "cha'nan'jie"),
    );
}

/// 用户词层的模糊命中同口径：系统词库里没有的「瓶安」只在用户层（规范码 `ping|an`），
/// 高权重压过系统词「平安」成为首选，编码栏同样按它走的 `pin|an` 显示。
#[test]
fn in_ing_user_word_preedit_follows_typed_segmentation() {
    if !has_pinyin() {
        return;
    }
    // (输入, 用户词, 规范码, 开模糊音时的编码栏, 关模糊音时的首选与编码栏)
    let cases = [("pinan", "瓶安", "pingan", "pin'an", ("拼按", "pin'an"))];
    for (input, word, code, want, plain) in cases {
        for on in [true, false] {
            let db =
                std::env::temp_dir().join(format!("wind_fuzzy_preedit_user_{input}_{on}.redb"));
            let _ = std::fs::remove_file(&db);
            let store = Arc::new(Store::open(&db).unwrap());
            // 两个音节都在 4 字节以内：边界位 0 与 4。
            store
                .add_user_word("pinyin", code, word, 2_000_000_000, 0b1_0001)
                .unwrap();
            let mut cfg = Config::default();
            cfg.schema.available = vec!["pinyin".into()];
            cfg.schema.active = "pinyin".into();
            cfg.input.default.chinese_mode = true;
            cfg.schema.pinyin.fuzzy.enabled = on;
            cfg.schema.pinyin.fuzzy.in_ing = true;
            // 关掉整句用用户词：否则「瓶安」先由词图的用户词模糊节点组成整句（边界由解码器
            // 回填），用户层词候选那一路（`search_store_fuzzy`）就测不到了。
            cfg.schema.pinyin.sentence_uses_user_words = false;
            let coord = Coordinator::new_headless_with_store(cfg, Some(&data_dir()), store);
            type_keys(&coord, input);
            let cands = coord.debug_all_candidate_texts();
            let preedit = coord.debug_preedit();
            drop(coord);
            let _ = std::fs::remove_file(&db);
            let (top, want) = if on { (word, want) } else { plain };
            assert_eq!(
                cands.first().map(String::as_str),
                Some(top),
                "前提：模糊音 {on} 时 {input} 首选应是「{top}」，实际: {:?}",
                &cands[..cands.len().min(5)]
            );
            assert_eq!(
                preedit, want,
                "模糊音 {on} 时 {input} 首选「{top}」的编码栏"
            );
        }
    }
}
