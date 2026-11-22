//! 换位纠错只能**加**候选：关纠错时有的候选文字，开纠错后必须仍在（前 50 条）。
//!
//! 纠错拼写注册成切分边后，曾挤掉同一跨度上的合法解释（`xign` 的「习惯」、`yuo` 的「与哦」）。
//!
//! 词库不存在时跳过。

use std::path::PathBuf;

use wind_config::Config;
use wind_engine::EngineManager;

fn data_dir() -> Option<PathBuf> {
    let p = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../build_dev/data");
    p.join("schemas/pinyin/cn_dicts/base.dict.yaml")
        .exists()
        .then_some(p)
}

fn texts(dir: &std::path::Path, correction: bool, input: &str, limit: usize) -> Vec<String> {
    cands(dir, correction, input, limit)
        .into_iter()
        .map(|(t, _)| t)
        .collect()
}

/// (文字, 是否经纠错)。
fn cands(
    dir: &std::path::Path,
    correction: bool,
    input: &str,
    limit: usize,
) -> Vec<(String, bool)> {
    let mut cfg = Config::default();
    cfg.schema.available = vec!["pinyin".to_string()];
    cfg.schema.active = "pinyin".to_string();
    cfg.input.correction.enabled = correction;
    cfg.input.correction.rules.gn_ng = true; // 出厂关；用例含 gogn / xign
    let mgr = EngineManager::new(&cfg, Some(dir));
    mgr.convert_with("pinyin", input, limit)
        .candidates
        .into_iter()
        .map(|c| (c.text, c.is_corrected))
        .collect()
}

/// 关纠错时前 50 条里的每条候选，开纠错后：(a) 仍在（允许被挤到 50 条之后，故开纠错侧取
/// 1000 条）；(b) 彼此相对顺序不变。
///
/// (b) 不含开纠错后改走纠错读法的文字：同文的简拼读法只吃前几个键，纠错读法吃满整串时
/// 换成后者（`gogn` 的「工」），它从简拼层挪到纠错层是有意的。
#[test]
fn correction_never_removes_candidates() {
    let Some(dir) = data_dir() else {
        eprintln!("跳过：无 build_dev/data 词库");
        return;
    };
    let mut lost = Vec::new();
    let mut reordered = Vec::new();
    // 前 8 条是回归现场；其后是排查中各机制各自的现场（混合简拼降级、同文占位、纠错召回）。
    let inputs = [
        "yuo", "yone", "xign", "gogn", "xiong", "zhong", "nage", "liou", "zhon", "boa", "gognping",
        "zhonguo", "hia", "xignfu",
    ];
    for input in inputs {
        let off = texts(&dir, false, input, 50);
        let on_all = cands(&dir, true, input, 1000);
        let on: Vec<String> = on_all.iter().map(|(t, _)| t.clone()).collect();
        let moved: Vec<&String> = on_all.iter().filter(|(_, c)| *c).map(|(t, _)| t).collect();
        for t in &off {
            if !on.contains(t) {
                lost.push(format!("{input}: {t}"));
            }
        }
        let stays = |t: &&String| !moved.contains(t);
        let kept: Vec<&String> = on
            .iter()
            .filter(|t| off.contains(t))
            .filter(stays)
            .collect();
        let expect: Vec<&String> = off
            .iter()
            .filter(|t| on.contains(t))
            .filter(stays)
            .collect();
        if kept != expect {
            reordered.push(input);
        }
    }
    assert!(lost.is_empty(), "开纠错后消失的候选: {lost:?}");
    assert!(
        reordered.is_empty(),
        "非纠错候选相对顺序变了: {reordered:?}"
    );
}

/// 守卫不能靠「纠错什么都不出」空过：该加的纠错候选仍在。
#[test]
fn correction_still_adds_candidates() {
    let Some(dir) = data_dir() else {
        eprintln!("跳过：无 build_dev/data 词库");
        return;
    };
    for (input, text, limit) in [
        ("gogn", "工", 50),
        ("gognping", "公平", 50),
        ("hia", "还", 50),
        ("zhonguo", "中国", 50),
        ("dagnshi", "当时", 50),
    ] {
        let t = text.to_string();
        assert!(!texts(&dir, false, input, limit).contains(&t), "{input}");
        assert!(texts(&dir, true, input, limit).contains(&t), "{input}");
    }
}
