//! 双拼击键坐标整句图（`ConvertOptions::sp_raw_lattice`）接入 step 2 / 2c 的对拍。
//!
//! - 开关打开：干净整句的首选与旧路径一致，整句 `consumed_length` 直接是击键数。
//! - 开关关闭：输出与接入前逐条一致（快照取自接入前的提交，见 `OFF_BASELINE`）。
//!
//! 走真词库（`build_dev/data`），缺失时跳过。

use std::path::{Path, PathBuf};

use wind_config::Config;
use wind_engine::pinyin::shuangpin::{Layout, ShuangpinConverter, ShuangpinReverse};
use wind_engine::{ConvertOptions, EngineManager};

fn data_dir() -> Option<PathBuf> {
    let p = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../build_dev/data");
    p.join("schemas/pinyin/cn_dicts/base.dict.yaml")
        .exists()
        .then_some(p)
}

/// 出厂 `shuangpin.schema.toml` 即小鹤。
fn manager(dir: &Path) -> EngineManager {
    let mut cfg = Config::default();
    cfg.schema.available = vec!["shuangpin".to_string()];
    cfg.schema.active = "shuangpin".to_string();
    EngineManager::new(&cfg, Some(dir))
}

fn xiaohe_reverse() -> ShuangpinReverse {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../../data/schemas/shuangpin/xiaohe.toml");
    let layout = Layout::from_toml(&path).expect("加载小鹤布局");
    ShuangpinConverter::new(layout).build_reverse()
}

fn convert(mgr: &EngineManager, keys: &str, raw: bool) -> Vec<(String, usize)> {
    let opts = ConvertOptions {
        sp_raw_lattice: raw,
        ..Default::default()
    };
    mgr.convert_with_opts("shuangpin", keys, 30, opts)
        .candidates
        .into_iter()
        .map(|c| (c.text, c.consumed_length))
        .collect()
}

/// 取自 `grammar_sentence_eval` 的整句，按音节写出再转小鹤击键。
const CLEAN: &[&str] = &[
    "wo shi zhong guo ren",
    "jin tian tian qi hen hao",
    "wo men yi qi qu chi fan",
    "xie xie ni de bang zhu",
    "zhe ge wen ti hen zhong yao",
    "qing wen xian zai ji dian",
    "wo men xu yao geng duo shi jian",
    "ta zheng zai kan shu",
    "zhong hua ren min gong he guo",
    "ji suan ji ke xue",
    "ren gong zhi neng",
    "gong zuo bao gao",
    "chi fan le ma",
    "ming tian zai jian",
    "wo yao qu shang ban",
    "zhe shi yi ge hao ban fa",
    "wo bu zhi dao zen me ban",
    "ni zuo tian qu na li le",
    "wo ming tian yao kai hui",
    "zhe ben shu hen you yi si",
    "wo de peng you lai le",
    "yi jing wan cheng le",
];

fn keys_of(rev: &ShuangpinReverse, s: &str) -> String {
    let syls: Vec<&str> = s.split(' ').collect();
    rev.encode_all(&syls)
        .unwrap_or_else(|| panic!("小鹤编码失败: {s}"))
}

#[test]
fn raw_lattice_sentence_matches_old_on_clean_inputs() {
    let Some(dir) = data_dir() else {
        eprintln!("跳过：拼音词库不存在");
        return;
    };
    let mgr = manager(&dir);
    let rev = xiaohe_reverse();
    let mut checked = 0;
    for s in CLEAN {
        let keys = keys_of(&rev, s);
        let off = convert(&mgr, &keys, false);
        let on = convert(&mgr, &keys, true);
        let (off_top, on_top) = (off.first().expect("关"), on.first().expect("开"));
        assert_eq!(on_top.0, off_top.0, "{s} ({keys}) 首选不一致");
        assert_eq!(
            on_top.1,
            keys.len(),
            "{s} ({keys}) 开关打开时整句应消费全部击键"
        );
        checked += 1;
    }
    eprintln!("raw_lattice_sentence_matches_old_on_clean_inputs: {checked} 条");
    assert_eq!(checked, CLEAN.len());
}

/// 关闭时的输入：干净整句、尾部落单键（step 2c）、手动分隔符、拼不出的键对、短输入。
const OFF_INPUTS: &[&str] = &[
    "nihc",
    "nihcm",
    "ni'hc",
    "n'hc",
    "buvidc",
    "buvidck",
    "woui",
    "wouivsgo",
    "wouivsgor",
    "jbtmtmqihfhc",
    "womfyiqiquiifj",
    "zhuuuiuu",
    "uiuu",
    "dajx",
    "pl",
    "nipl",
    "a",
    "ab",
    "wo",
    "wom",
    "yrbe",
    "yrbek",
    "ggvo",
    "ggvobcgc",
    "vsgo",
    "vsgorf",
    "qihz",
    "qihztevg",
    "nikeyibhwokjkjma",
    "xmui",
];

/// 由接入前的提交（89cb1280）跑 `dump_off_baseline` 生成：每条取前 5 个 (文字, consumed)。
const OFF_BASELINE: &[(&str, &[(&str, usize)])] = &[
    (
        "nihc",
        &[("你好", 4), ("拟好", 4), ("你", 2), ("尼", 2), ("逆", 2)],
    ),
    (
        "nihcm",
        &[("你好", 4), ("你好吗", 5), ("你", 2), ("尼", 2), ("逆", 2)],
    ),
    (
        "ni'hc",
        &[("你好", 5), ("拟好", 5), ("你", 2), ("尼", 2), ("逆", 2)],
    ),
    (
        "n'hc",
        &[
            ("你好", 4),
            ("您好", 4),
            ("内耗", 4),
            ("能耗", 4),
            ("年号", 4),
        ],
    ),
    (
        "buvidc",
        &[
            ("不知道", 6),
            ("不", 2),
            ("不知", 4),
            ("部", 2),
            ("不止", 4),
        ],
    ),
    (
        "buvidck",
        &[
            ("不知道", 6),
            ("不知道看", 7),
            ("不", 2),
            ("不知", 4),
            ("部", 2),
        ],
    ),
    (
        "woui",
        &[
            ("我是", 4),
            ("卧室", 4),
            ("我市", 4),
            ("卧式", 4),
            ("我试", 4),
        ],
    ),
    (
        "wouivsgo",
        &[
            ("我是中国", 8),
            ("我", 2),
            ("我是", 4),
            ("卧室", 4),
            ("喔", 2),
        ],
    ),
    (
        "wouivsgor",
        &[
            ("我是中国", 8),
            ("我是中国人", 9),
            ("我", 2),
            ("我是", 4),
            ("卧室", 4),
        ],
    ),
    (
        "jbtmtmqihfhc",
        &[
            ("今天天气很好", 12),
            ("今天", 4),
            ("进", 2),
            ("近", 2),
            ("仅", 2),
        ],
    ),
    (
        "womfyiqiquiifj",
        &[
            ("我们一起去吃饭", 14),
            ("我", 2),
            ("我们", 4),
            ("喔", 2),
            ("我们一起", 8),
        ],
    ),
    (
        "zhuuuiuu",
        &[("脏舒适书", 8), ("脏", 2), ("葬", 2), ("臧", 2), ("藏", 2)],
    ),
    (
        "uiuu",
        &[
            ("史书", 4),
            ("实属", 4),
            ("实数", 4),
            ("诗书", 4),
            ("师叔", 4),
        ],
    ),
    (
        "dajx",
        &[
            ("大家", 4),
            ("打架", 4),
            ("打假", 4),
            ("大贾", 4),
            ("大驾", 4),
        ],
    ),
    (
        "pl",
        &[
            ("评论", 2),
            ("频率", 2),
            ("漂亮", 2),
            ("跑了", 2),
            ("拍了", 2),
        ],
    ),
    (
        "nipl",
        &[("你", 2), ("尼", 2), ("逆", 2), ("拟", 2), ("泥", 2)],
    ),
    (
        "a",
        &[("啊", 1), ("按", 1), ("爱", 1), ("哎", 1), ("唉", 1)],
    ),
    (
        "ab",
        &[
            ("暗部", 2),
            ("岸本", 2),
            ("安保", 2),
            ("岸边", 2),
            ("澳币", 2),
        ],
    ),
    (
        "wo",
        &[("我", 2), ("喔", 2), ("窝", 2), ("握", 2), ("沃", 2)],
    ),
    (
        "wom",
        &[
            ("我们", 3),
            ("我妈", 3),
            ("我没", 3),
            ("我买", 3),
            ("我妹", 3),
        ],
    ),
    (
        "yrbe",
        &[("原本", 4), ("院本", 4), ("元", 2), ("原", 2), ("远", 2)],
    ),
    (
        "yrbek",
        &[("元", 2), ("原", 2), ("远", 2), ("愿", 2), ("圆", 2)],
    ),
    (
        "ggvo",
        &[("更桌", 4), ("更", 2), ("梗", 2), ("羹", 2), ("耿", 2)],
    ),
    (
        "ggvobcgc",
        &[("更桌报告", 8), ("更", 2), ("梗", 2), ("羹", 2), ("耿", 2)],
    ),
    (
        "vsgo",
        &[("中国", 4), ("种过", 4), ("种果", 4), ("中", 2), ("种", 2)],
    ),
    (
        "vsgorf",
        &[("中国人", 6), ("中", 2), ("中国", 4), ("种", 2), ("重", 2)],
    ),
    (
        "qihz",
        &[("气候", 4), ("其后", 4), ("期后", 4), ("其", 2), ("起", 2)],
    ),
    (
        "qihztevg",
        &[("气候特征", 8), ("其", 2), ("起", 2), ("期", 2), ("气", 2)],
    ),
    (
        "nikeyibhwokjkjma",
        &[
            ("你可以帮我看看吗", 16),
            ("你", 2),
            ("你可以", 6),
            ("尼", 2),
            ("逆", 2),
        ],
    ),
    (
        "xmui",
        &[
            ("现实", 4),
            ("显示", 4),
            ("先是", 4),
            ("限时", 4),
            ("现世", 4),
        ],
    ),
];

#[test]
fn raw_lattice_off_equals_baseline() {
    let Some(dir) = data_dir() else {
        eprintln!("跳过：拼音词库不存在");
        return;
    };
    let mgr = manager(&dir);
    assert_eq!(OFF_BASELINE.len(), OFF_INPUTS.len(), "快照与输入条数不符");
    for (keys, want) in OFF_BASELINE {
        let got = convert(&mgr, keys, false);
        let got: Vec<(&str, usize)> = got.iter().take(5).map(|(t, n)| (t.as_str(), *n)).collect();
        assert_eq!(&got[..], *want, "{keys} 关闭时输出变了");
    }
}

/// step 2c（残码整句）在双拼下照常触发：旧注释称双拼跳过，门条件实际不排除双拼
/// （`query` 是含残码声母的全拼）。关掉 2c 后「不知道看」消失，可见它出自 2c。
#[test]
fn step_2c_fires_on_double_pinyin() {
    let Some(dir) = data_dir() else {
        eprintln!("跳过：拼音词库不存在");
        return;
    };
    let mgr = manager(&dir);
    for raw in [false, true] {
        let with = |allow: Option<bool>| {
            let opts = ConvertOptions {
                sp_raw_lattice: raw,
                allow_partial_final: allow,
                ..Default::default()
            };
            mgr.convert_with_opts("shuangpin", "buvidck", 30, opts)
                .candidates
                .into_iter()
                .map(|c| (c.text, c.consumed_length))
                .collect::<Vec<_>>()
        };
        let on = with(Some(true));
        assert!(
            on.contains(&("不知道看".to_string(), 7)),
            "raw={raw}: 2c 应出「不知道看」消费 7 键，实际 {:?}",
            &on[..on.len().min(6)]
        );
        let off = with(Some(false));
        assert!(
            !off.iter().any(|(t, _)| t == "不知道看"),
            "raw={raw}: 关掉 2c 后不应再有「不知道看」"
        );
    }
}

/// 旧 2c 在转换后的全拼上用 DAG 重切：小鹤 `fjgj` 是 fan|gan，`fangank` 却按 fang|an
/// 组出「方案…」，随后被双拼边界校验删掉，残码整句一条不剩。击键图只走真值键对。
#[test]
fn raw_lattice_2c_keeps_true_segmentation() {
    let Some(dir) = data_dir() else {
        eprintln!("跳过：拼音词库不存在");
        return;
    };
    let mgr = manager(&dir);
    let off = convert(&mgr, "fjgjk", false);
    assert!(
        !off.iter().any(|(t, n)| *n == 5 && t.starts_with("反感")),
        "前提：旧路径没有「反感+残码」整句，实际 {:?}",
        &off[..off.len().min(6)]
    );
    let on = convert(&mgr, "fjgjk", true);
    assert!(
        on.iter().any(|(t, n)| *n == 5 && t.starts_with("反感")),
        "击键图应出「反感+残码」整句（消费 5 键），实际 {:?}",
        &on[..on.len().min(6)]
    );
}

/// 重新生成 `OFF_BASELINE`：`cargo test --test shuangpin_raw_lattice dump_off_baseline -- --ignored --nocapture`。
#[test]
#[ignore = "快照生成工具：依赖 build_dev 真实词库"]
fn dump_off_baseline() {
    let Some(dir) = data_dir() else {
        eprintln!("跳过：拼音词库不存在");
        return;
    };
    let mgr = manager(&dir);
    for keys in OFF_INPUTS {
        let got = convert(&mgr, keys, false);
        let items: Vec<String> = got
            .iter()
            .take(5)
            .map(|(t, n)| format!("({t:?}, {n})"))
            .collect();
        println!("    ({keys:?}, &[{}]),", items.join(", "));
    }
}
