//! 整音节输入跳过 step 4 词库前缀补全（[`super::prefix_completion_is_exact_only`]）。
//!
//! 两类守卫：
//! - 对拍：同一输入在「跳过」与「照旧扫描」（`test_hooks::set_never_skip_step4`）两条路径下
//!   `ConvertResult` 逐字段一致（比 `Debug` 输出）。合成词库版随 CI 跑；真实词库版依赖
//!   `build_dev/data`，`#[ignore]`。
//! - 计数护栏：step 4 实际发起的词库前缀扫描次数（确定性，不看墙钟）。整音节短输入必须是 0，
//!   残码 / 调低门槛时必须是 1，放宽或收紧跳过条件都会被它抓到。

use super::shuangpin::{Layout, ShuangpinConverter};
use super::test_hooks::{set_never_skip_step4, take_step4_prefix_scans};
use super::*;
use wind_dict::codetable::CodetableDict;

/// 对拍一次：返回 (跳过路径的 Debug, 基线路径的 Debug, 跳过路径是否真的省掉了扫描)。
fn run_both(convert: &dyn Fn(&str) -> String, input: &str) -> (String, String, bool) {
    set_never_skip_step4(false);
    take_step4_prefix_scans();
    let skipped = convert(input);
    let scans_skip = take_step4_prefix_scans();
    set_never_skip_step4(true);
    let baseline = convert(input);
    let scans_base = take_step4_prefix_scans();
    set_never_skip_step4(false);
    (skipped, baseline, scans_skip < scans_base)
}

/// 对一批输入对拍，返回 (总数, 实际跳过数)；有差异直接 panic 并列出前几条。
fn assert_parity(
    label: &str,
    convert: &dyn Fn(&str) -> String,
    inputs: &[String],
) -> (usize, usize) {
    let mut diffs = Vec::new();
    let mut skipped = 0;
    for input in inputs {
        let (a, b, s) = run_both(convert, input);
        skipped += usize::from(s);
        if a != b {
            diffs.push(format!("`{input}`\n  跳过: {a}\n  基线: {b}"));
        }
    }
    assert!(
        diffs.is_empty(),
        "[{label}] {} / {} 条输入跳过前后不一致，前 3 条:\n{}",
        diffs.len(),
        inputs.len(),
        diffs.iter().take(3).cloned().collect::<Vec<_>>().join("\n")
    );
    (inputs.len(), skipped)
}

/// 1~`max_len` 位小写字母穷举。
fn letter_strings(max_len: usize) -> Vec<String> {
    let mut out = Vec::new();
    let mut layer = vec![String::new()];
    for _ in 0..max_len {
        layer = layer
            .iter()
            .flat_map(|p| (b'a'..=b'z').map(move |c| format!("{p}{}", c as char)))
            .collect();
        out.extend(layer.iter().cloned());
    }
    out
}

fn yaml_engine(tag: &str, cfg: Config, entries: &[(&str, &str, i32)]) -> PinyinEngine {
    use std::io::Write;
    let path = std::env::temp_dir().join(format!("wind_a314_{tag}.dict.yaml"));
    {
        let mut f = std::fs::File::create(&path).unwrap();
        writeln!(f, "---\nname: py\n...").unwrap();
        for (text, code, w) in entries {
            writeln!(f, "{text}\t{code}\t{w}").unwrap();
        }
    }
    let mut dict = CodetableDict::load(&path).unwrap();
    let _ = std::fs::remove_file(&path);
    // 无边界真值的条目（用户手输码 / 旧词典 / 导入脏数据）：词库层判不了、一律放行，
    // 是「注定为空」论证里唯一的漏口，夹具必须带上。
    dict.merge_single("xiandai".into(), "现代·无边界".into(), 500, 0);
    dict.merge_single("ni".into(), "你·无边界".into(), 500, 0);
    dict.merge_single("nihaoma".into(), "你好吗·无边界".into(), 500, 0);
    dict.merge_single("huangdi".into(), "黄帝·无边界".into(), 500, 0);
    dict.merge_single("zhongwen".into(), "中文·无边界".into(), 500, 0);
    PinyinEngine::new(cfg, CachedDict::Memory(dict))
}

/// 合成夹具：单字、精确词、多音节补全、切分比 DAG 更粗的词（`xian|ren` 对 `xi'an`）、
/// 同码多义（`xian` = 先 / 西安）、w≤0 条目。
const ENTRIES: &[(&str, &str, i32)] = &[
    ("你", "ni", 900),
    ("泥", "ni", 300),
    ("您", "nin", 800),
    ("你好", "ni hao", 700),
    ("你好吗", "ni hao ma", 400),
    ("你们好", "ni men hao", 200),
    ("西", "xi", 800),
    ("先", "xian", 800),
    ("西安", "xi an", 600),
    ("先人", "xian ren", 300),
    ("西安人", "xi an ren", 100),
    ("现代", "xian dai", 700),
    ("湖", "hu", 800),
    ("胡", "hu", 500),
    ("互联网", "hu lian wang", 600),
    ("中", "zhong", 900),
    ("中国", "zhong guo", 900),
    ("中国人", "zhong guo ren", 500),
    ("中国人民", "zhong guo ren min", 300),
    ("方案", "fang an", 500),
    ("反感", "fan gan", 300),
    ("啊", "a", 900),
    ("爱", "ai", 800),
    ("爱国", "ai guo", 400),
    ("存疑", "ni hao ya", 0),
];

fn extra_inputs() -> Vec<String> {
    [
        "nihao",
        "nihaoma",
        "nimen",
        "xian",
        "xi'an",
        "xianren",
        "xiandai",
        "zhong",
        "zhongguo",
        "zhongguoren",
        "zhongguorenmin",
        "fangan",
        "fang'an",
        "fan'gan",
        "hulianwang",
        "aiguo",
        "nih",
        "xia",
        "zhongg",
        "meiy",
        "ni'hao",
        "n'h",
        "nh",
        "huang",
        "huangdi",
        "zhongwen",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect()
}

fn fuzzy_all() -> FuzzyConfig {
    FuzzyConfig {
        zh_z: true,
        ch_c: true,
        sh_s: true,
        n_l: true,
        f_h: true,
        r_l: true,
        an_ang: true,
        en_eng: true,
        in_ing: true,
        ian_iang: true,
        uan_uang: true,
        un_ong: true,
        eng_ong: true,
        ..Default::default()
    }
}

fn debug_of(e: &PinyinEngine) -> impl Fn(&str) -> String + '_ {
    move |s: &str| format!("{:?}", e.convert(s, 300).unwrap())
}

#[test]
fn step4_skip_matches_full_scan_on_synthetic_dict() {
    let mut inputs = letter_strings(3);
    inputs.extend(extra_inputs());

    let plain = yaml_engine("plain", Config::default(), ENTRIES);
    let (_, skipped) = assert_parity("出厂", &debug_of(&plain), &inputs);
    assert!(skipped > 0, "对拍须真的覆盖到跳过分支");

    let fuzzy = yaml_engine("fuzzy", Config::default(), ENTRIES).with_fuzzy(fuzzy_all());
    assert_parity("模糊音全开", &debug_of(&fuzzy), &inputs);

    let schema_dir =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../data/schemas/shuangpin");
    let layout = Layout::from_toml(&schema_dir.join("xiaohe.toml")).expect("加载小鹤布局失败");
    let cfg = Config {
        allow_full_pinyin: true,
        ..Config::default()
    };
    let sp = yaml_engine("sp", cfg, ENTRIES).with_shuangpin(ShuangpinConverter::new(layout));
    // 小鹤：ni=ni, hk=hao, xi=xi, xm=xian, an=an, rf=ren, vs=zhong, go=guo
    let mut sp_inputs = letter_strings(3);
    sp_inputs.extend(
        [
            "nihk", "xian", "xm", "xmrf", "xianrf", "vs", "vsgo", "vsgorf", "nihkma", "nihao",
        ]
        .iter()
        .map(|s| s.to_string()),
    );
    assert_parity("小鹤双拼+全拼降级", &debug_of(&sp), &sp_inputs);
}

/// 计数护栏：跳过条件的两个边都钉住。
///
/// - 放宽（残码位也跳 / 多音节整音节也跳 / 不看 cap）→ `nih`、`nihao`、`xi'an`、
///   relaxed 配置下的 `ni` 扫描数变 0，红（多音节那条为何不能跳，见被测函数文档）；
/// - 收紧（不再跳 / 只跳部分单音节）→ `ni`/`xi`/`hu`/`zhong` 扫描数变 1，红。
#[test]
fn step4_prefix_scan_count_guard() {
    let plain = yaml_engine("guard", Config::default(), ENTRIES);
    let scans = |e: &PinyinEngine, s: &str| {
        set_never_skip_step4(false);
        take_step4_prefix_scans();
        e.convert(s, 300).unwrap();
        take_step4_prefix_scans()
    };
    // 单音节整音节、cap == 1：注定只剩精确匹配 ⇒ 不扫。
    for s in ["ni", "xi", "hu", "zhong", "xian", "a", "ai"] {
        assert_eq!(scans(&plain, s), 0, "单音节 `{s}` 不应再扫 step 4 前缀子树");
    }
    // 残码位：前缀补全的本职（`nih`→「你好」），必须扫。
    for s in ["n", "x", "zh", "nih", "zhongg", "xia'n"] {
        assert_eq!(scans(&plain, s), 1, "残码 `{s}` 必须照常扫 step 4");
    }
    // 多音节整音节：不是注定为空（`xi'an` 出「先人」），必须扫。
    for s in ["nihao", "zhongguo", "xi'an"] {
        assert_eq!(scans(&plain, s), 1, "多音节 `{s}` 必须照常扫 step 4");
    }
    assert!(
        texts_of(&plain, "xi'an").contains(&"先人".to_string()),
        "对照：`xi'an` 的 step 4 确有产出，上面那组 1 守的是真实输出"
    );
    // 用户把门槛调低 ⇒ cap > started，补全合法地多出音节，必须扫。
    let relaxed = yaml_engine(
        "guard_relaxed",
        Config {
            completion_min_syllables: 1,
            ..Config::default()
        },
        ENTRIES,
    );
    for s in ["ni", "xi", "nihao"] {
        assert_eq!(
            scans(&relaxed, s),
            1,
            "min_syllables=1 下 `{s}` 必须扫 step 4"
        );
    }
    assert!(
        texts_of(&relaxed, "xi").contains(&"西安".to_string()),
        "对照：放开门槛后 `xi` 的补全确实有产出，说明上面那组 1 不是空转"
    );
}

fn texts_of(e: &PinyinEngine, s: &str) -> Vec<String> {
    e.convert(s, 300)
        .unwrap()
        .candidates
        .into_iter()
        .map(|c| c.text)
        .collect()
}

// ── 真实词库对拍（依赖 build_dev/data）──

fn real_data_dir() -> Option<std::path::PathBuf> {
    let d = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../build_dev/data");
    d.join("schemas/pinyin").is_dir().then_some(d)
}

/// 真实词库里的整音节击键：全部单音节码 + 按固定步长抽样的 2~4 音节词（及其 `'` 分隔形态）。
fn real_dict_syllable_inputs(dir: &std::path::Path) -> (Vec<Vec<String>>, Vec<String>) {
    let text = std::fs::read_to_string(dir.join("schemas/pinyin/cn_dicts/base.dict.yaml")).unwrap();
    let mut singles = std::collections::BTreeSet::new();
    let mut multi = Vec::new();
    let mut n = 0usize;
    for line in text.lines().skip_while(|l| *l != "...") {
        let mut cols = line.split('\t');
        let (Some(_), Some(py)) = (cols.next(), cols.next()) else {
            continue;
        };
        let syls: Vec<String> = py.split(' ').map(str::to_string).collect();
        if syls
            .iter()
            .any(|s| !s.bytes().all(|b| b.is_ascii_lowercase()))
        {
            continue;
        }
        match syls.len() {
            1 => {
                singles.insert(syls[0].clone());
            }
            2..=4 => {
                n += 1;
                if n.is_multiple_of(97) {
                    multi.push(syls);
                }
            }
            _ => {}
        }
    }
    let mut flat: Vec<String> = singles.into_iter().collect();
    flat.extend(multi.iter().map(|s| s.concat()));
    flat.extend(multi.iter().step_by(3).map(|s| s.join("'")));
    (multi, flat)
}

/// 真实词库逐字段对拍（出厂 / 模糊音全开 / 小鹤双拼+全拼降级）。
///
/// 跑法：`cargo test -p wind-engine --release --lib step4_skip_matches_full_scan_on_real_dict
/// -- --ignored --nocapture`
#[test]
#[ignore = "依赖 build_dev/data，且要跑数万次 convert"]
fn step4_skip_matches_full_scan_on_real_dict() {
    let Some(dir) = real_data_dir() else {
        eprintln!("跳过：找不到 build_dev/data");
        return;
    };
    let (multi, corpus) = real_dict_syllable_inputs(&dir);
    let mut inputs = letter_strings(3);
    inputs.extend(corpus);

    let base = wind_config::Config::load(Some(&dir)).unwrap_or_default();
    let mut fuzzy = base.clone();
    {
        let f = &mut fuzzy.schema.pinyin.fuzzy;
        f.enabled = true;
        f.zh_z = true;
        f.ch_c = true;
        f.sh_s = true;
        f.n_l = true;
        f.f_h = true;
        f.r_l = true;
        f.an_ang = true;
        f.en_eng = true;
        f.in_ing = true;
        f.ian_iang = true;
        f.uan_uang = true;
        f.un_ong = true;
        f.eng_ong = true;
    }
    for (label, cfg) in [("出厂", base.clone()), ("模糊音全开", fuzzy)] {
        let mgr = crate::EngineManager::new(&cfg, Some(&dir));
        let conv = |s: &str| format!("{:?}", mgr.convert(s, 300));
        let (total, skipped) = assert_parity(label, &conv, &inputs);
        eprintln!("[{label}] 全拼 {total} 条一致，其中 {skipped} 条走了跳过分支");
    }

    let mut sp = base;
    sp.schema.active = "shuangpin".into();
    sp.schema.available = vec!["shuangpin".into()];
    sp.schema.pinyin.shuangpin.allow_full_pinyin = true;
    let mgr = crate::EngineManager::new(&sp, Some(&dir));
    let mut sp_inputs = letter_strings(3);
    sp_inputs.extend(multi.iter().filter_map(|syls| {
        let refs: Vec<&str> = syls.iter().map(String::as_str).collect();
        mgr.shuangpin_code_of_syllables(&refs)
    }));
    let conv = |s: &str| format!("{:?}", mgr.convert(s, 300));
    let (total, skipped) = assert_parity("双拼+全拼降级", &conv, &sp_inputs);
    eprintln!("[双拼+全拼降级] {total} 条一致，其中 {skipped} 条走了跳过分支");
}
