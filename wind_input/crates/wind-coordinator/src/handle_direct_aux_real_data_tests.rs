//! 真实数据（`build_dev/data`：小鹤双拼 + 雾凇词库 + `flypy_full.txt`），缺数据跳过。
//! 放在 crate 内而不是 `tests/`：要看候选的 `is_direct_aux` 标记，区分「被直接辅助提上来」
//! 与「本来就排在那儿」。
use crate::coordinator::Coordinator;
use std::sync::Arc;
use wind_bridge::handler::{KeyEventData, MessageHandler};
use wind_config::Config;
use wind_ipc::protocol::EVENT_KEY_DOWN;
use wind_keys::keymap;

fn data_dir() -> std::path::PathBuf {
    std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../build_dev/data")
}

fn has_data() -> bool {
    let d = data_dir().join("schemas");
    let ok = d.join("shuangpin.schema.toml").is_file()
        && d.join("aux_code/flypy_full.txt").is_file()
        && d.join("pinyin/rime_frost.dict.yaml").is_file();
    if !ok {
        eprintln!(
            "跳过：build_dev/data 缺小鹤双拼方案 / flypy_full.txt / 雾凇词库（先跑 gen-data）"
        );
    }
    ok
}

fn coord(direct: bool) -> Arc<Coordinator> {
    let mut cfg = Config::default();
    cfg.schema.available = vec!["shuangpin".into()];
    cfg.schema.active = "shuangpin".into();
    cfg.input.default.chinese_mode = true;
    cfg.schema.pinyin.aux_code.enabled = true;
    cfg.schema.pinyin.aux_code.direct = direct;
    Coordinator::new_headless(cfg, Some(&data_dir()))
}

/// `direct` 开 + 逃生口（`[engine.aux_code] continuous = false`）：只做串尾直接辅助。
fn coord_tail() -> Arc<Coordinator> {
    static N: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    let n = N.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let ov = std::env::temp_dir().join(format!(
        "wind_direct_aux_tail_ov_{}_{n}",
        std::process::id()
    ));
    std::fs::create_dir_all(&ov).unwrap();
    std::fs::write(
        ov.join("shuangpin.toml"),
        "[engine.aux_code]\ncontinuous = false\n",
    )
    .unwrap();
    let mut cfg = Config::default();
    cfg.schema.available = vec!["shuangpin".into()];
    cfg.schema.active = "shuangpin".into();
    cfg.input.default.chinese_mode = true;
    cfg.schema.pinyin.aux_code.enabled = true;
    cfg.schema.pinyin.aux_code.direct = true;
    Coordinator::new_headless_with_override(cfg, Some(&data_dir()), Some(ov))
}

fn press(c: &Coordinator, vk: u32) {
    c.handle_key_event(&KeyEventData {
        key_code: vk,
        scan_code: 0,
        modifiers: 0,
        event_type: EVENT_KEY_DOWN,
        toggles: 0,
        event_seq: 0,
        prev_char: 0,
        seq_char: 0,
    });
}

/// 清空组合后逐键敲入 `input`，每键后回调一次（参数为已敲前缀）。
fn type_each(c: &Coordinator, input: &str, mut each: impl FnMut(&str)) {
    press(c, keymap::VK_ESCAPE);
    for (i, ch) in input.char_indices() {
        press(c, keymap::VK_A + (ch as u32 - 'a' as u32));
        each(&input[..i + 1]);
    }
}

/// 候选（文本, 是否直接辅助命中）。
fn cands(c: &Coordinator) -> Vec<(String, bool)> {
    c.state
        .lock()
        .unwrap()
        .candidates
        .iter()
        .map(|c| (c.text.clone(), c.is_direct_aux))
        .collect()
}

fn after(c: &Coordinator, input: &str) -> Vec<(String, bool)> {
    type_each(c, input, |_| {});
    cands(c)
}

fn top5(v: &[(String, bool)]) -> Vec<String> {
    v.iter()
        .take(5)
        .map(|(t, hit)| if *hit { format!("{t}*") } else { t.clone() })
        .collect()
}

fn first(v: &[(String, bool)]) -> Option<&str> {
    v.first().map(|(t, _)| t.as_str())
}

/// §11 真实数据示例（串尾直接辅助，逃生口下）。`*` = 直接辅助命中项。开关两侧的前 5 名
/// 打到 stderr 供报告。
#[test]
fn xiaohe_examples() {
    if !has_data() {
        return;
    }
    let (on, off) = (coord_tail(), coord(false));
    for input in ["uidup", "uidupl", "goqkk", "goqkg", "xlr", "jxl", "jxlm"] {
        eprintln!(
            "{input}: on {:?} | off {:?}",
            top5(&after(&on, input)),
            top5(&after(&off, input))
        );
    }
    // 释 = pl；同音的湿 dy、适 zk、十 al、试 yg 都不以 p 开头。
    assert_eq!(first(&after(&on, "uidup")), Some("释读"));
    assert_ne!(first(&after(&off, "uidup")), Some("释读"), "反向对照");
    // `pl` 不是合法小鹤音节 ⇒ 无全音节候选可保留，命中项直接居首。
    assert_eq!(first(&after(&on, "uidupl")), Some("释读"));
    // 国 = ky：1 位 k 国庆、国情都中；庆 = gd、情 = xo：g 只中国庆。
    let k = after(&on, "goqkk");
    for w in ["国庆", "国情"] {
        assert!(
            k.iter().any(|(t, hit)| t == w && *hit),
            "goqkk 应命中 {w}：{k:?}"
        );
    }
    let g = after(&on, "goqkg");
    assert!(g.iter().any(|(t, hit)| t == "国庆" && *hit), "{g:?}");
    assert!(!g.iter().any(|(t, hit)| t == "国情" && *hit), "{g:?}");
    assert_eq!(first(&g), Some("国庆"));
    // 像 = rn、架 = lm：短前缀奇数长度，命中项居首。
    assert_eq!(first(&after(&on, "xlr")), Some("像"));
    // `jxl`：1 位 `l` 同时中 加 lk、甲、架 lm、驾…，架是命中项之一但未必居首。
    // 补打第 2 位 `jxlm` 是偶数长度：「加练」「价廉」是覆盖全部输入的整词，按全音节优先
    // 保留在前 2 位，架（lm）接在其后。
    let l = after(&on, "jxl");
    assert!(l.iter().any(|(t, hit)| t == "架" && *hit), "{l:?}");
    let lm = after(&on, "jxlm");
    assert!(lm.iter().any(|(t, hit)| t == "架" && *hit), "{lm:?}");
}

/// 连续模式（`direct` 开）的真实数据示例：词中辅码交给引擎；串尾一步只认末字 / 单字，
/// 「词组任一字」不再提前（万象 Pro 同）。
#[test]
fn xiaohe_examples_continuous() {
    if !has_data() {
        return;
    }
    let on = coord(true);
    let hits = |v: &[(String, bool)]| -> Vec<String> {
        v.iter()
            .filter(|(_, h)| *h)
            .map(|(t, _)| t.clone())
            .collect()
    };
    // 词中辅码：国 = ky 跟在 go 后、你 = rx 跟在 ni 后。
    assert_eq!(first(&after(&on, "gokqk")), Some("国情"));
    assert_eq!(first(&after(&on, "nirxhc")), Some("你好"));
    // 末字 1 位：读 = yd、庆 = gd、情 = xo。
    let v = after(&on, "uiduy");
    assert_eq!(hits(&v)[..2], ["试读", "释读"], "{v:?}");
    assert_eq!(first(&after(&on, "goqkg")), Some("国庆"));
    assert_eq!(first(&after(&on, "goqkx")), Some("国情"));
    // 末字 2 位：引擎辅码整句即是，串尾同文去重。
    let v = after(&on, "goqkgd");
    assert_eq!(first(&v), Some("国庆"), "{v:?}");
    assert_eq!(v.iter().filter(|(t, _)| t == "国庆").count(), 1);
    // 单字 1 位：像 = rn。2 位（xlrn、jxlm）不提前，万象需 `/`。
    assert_eq!(first(&after(&on, "xlr")), Some("像"));
    for input in ["xlrn", "jxlm"] {
        let v = after(&on, input);
        assert!(hits(&v).is_empty(), "{input}: {v:?}");
    }
    // 任一字不再提前：释 = pl 是首字、国 = ky 是首字。
    for input in ["uidup", "uidupl", "goqkk"] {
        let v = after(&on, input);
        assert!(hits(&v).is_empty(), "{input}: {v:?}");
    }
}

/// 连打（取几键前的快照）与退格重打（快照作废、对前缀单独解码）给出同一批命中项。
///
/// 单音节前缀是高危区：`wu` 有数百个同音字，主候选在 limit 处截断，而兜底解码按辅码首字母
/// 在截断前准入——截断过的快照会漏掉排在后面的命中。
#[test]
fn typed_through_and_retyped_hits_agree() {
    if !has_data() {
        return;
    }
    let hits = |c: &Coordinator| -> Vec<String> {
        cands(c)
            .into_iter()
            .filter(|(_, h)| *h)
            .map(|(t, _)| t)
            .collect()
    };
    let single = ["wuk", "jip", "yuy", "jib", "yiy", "yil", "lik", "uiy"];
    let cases = [
        (coord_tail(), ["uidup", "goqkk", "uidupl", "jxlm"]),
        (coord(true), ["uiduy", "goqkg", "goqkx", "xlr"]),
    ];
    for (on, input) in cases
        .iter()
        .flat_map(|(c, words)| single.iter().chain(words).map(move |w| (c, *w)))
    {
        type_each(on, input, |_| {});
        let through = hits(on);
        press(on, keymap::VK_BACK);
        on.state.lock().unwrap().direct_aux_prev = None;
        let last = input.chars().last().unwrap();
        press(on, keymap::VK_A + (last as u32 - 'a' as u32));
        let retyped = hits(on);
        assert_eq!(through, retyped, "{input}: 连打与退格重打的命中项不一致");
        assert!(!through.is_empty(), "{input}: 没有命中，对照没测到东西");
    }
}

/// 检索范围（智能档）按码位看「同码位有没有常用字」：同码位的常用字（生 声 …，不以 h 起码）
/// 若不进池，生僻的 焺 就成了孤儿码位被放行，退格重打 `ugh` 会比连打多出一条命中并顶到首位。
#[test]
fn retyped_hits_respect_scope_filter() {
    if !has_data() {
        return;
    }
    for on in [coord_tail(), coord(true)] {
        type_each(&on, "ugh", |_| {});
        let through = cands(&on);
        press(&on, keymap::VK_BACK);
        on.state.lock().unwrap().direct_aux_prev = None;
        press(&on, keymap::VK_A + ('h' as u32 - 'a' as u32));
        let retyped = cands(&on);
        assert!(!retyped.iter().any(|(t, _)| t == "焺"), "{retyped:?}");
        assert_eq!(through, retyped);
    }
}

/// 6 音节整句逐键输入：偶数键首选与关闭直接辅助码时逐键一致（全音节解析优先），
/// 前缀 ≥ 3 音节的奇数键亦然（长句保首选）。
///
/// 变异对照（设计 §11）：偶数保留数改 0、奇数门槛改 99，本用例须变红。串尾（逃生口）与
/// 连续模式各跑一遍。
#[test]
fn sentences_keep_top_keystroke_by_keystroke() {
    if !has_data() {
        return;
    }
    let off = coord(false);
    for on in [coord_tail(), coord(true)] {
        keep_top_keystroke_by_keystroke(&on, &off);
    }
}

fn keep_top_keystroke_by_keystroke(on: &Coordinator, off: &Coordinator) {
    // 图书馆开放日 / 计算机语言学 / 动物园开门了（小鹤）。挑「前 3 音节是词、第 4 音节首键
    // 恰是其中某字的辅码开头」的句子（图=kd 配 kai、计=yu 配 yu、园=ke 配 kai），
    // 第 7 键前缀才真能命中，测得出「命中了也不抢长句首选」。
    let sentences = ["tuuugrkdfhri", "jisrjiyuyjxt", "dswuyrkdmfle"];
    let mut checked = 0;
    let mut total_hits = 0usize;
    for s in sentences {
        let mut want: Vec<Option<String>> = Vec::new();
        type_each(off, s, |_| {
            want.push(cands(off).first().map(|(t, _)| t.clone()))
        });
        let mut got: Vec<Option<String>> = Vec::new();
        let mut hits = 0usize;
        type_each(on, s, |typed| {
            let v = cands(on);
            let n = v.iter().filter(|(_, h)| *h).count();
            if n > 0 {
                eprintln!("  {typed}: {:?}", top5(&v));
            }
            hits += n;
            got.push(v.first().map(|(t, _)| t.clone()));
        });
        eprintln!("{s}: 整句 {:?}，逐键命中项合计 {hits}", want.last());
        total_hits += hits;
        for n in 3..=s.len() {
            // 短前缀（< 3 音节）奇数键按设计让位给命中项，不在约束内。门槛写死设计值 3
            // 而不引用常量：引用的话改常量会连带改掉本用例的检查范围，变异测不出来。
            if n % 2 == 1 && (n - 1) / 2 < 3 {
                continue;
            }
            assert_eq!(
                got[n - 1],
                want[n - 1],
                "{s} 第 {n} 键（{}）首选被直接辅助改动",
                &s[..n]
            );
            checked += 1;
        }
    }
    assert!(checked >= 20);
    assert!(total_hits > 0, "逐键一条命中都没有，对照没测到东西");
}

/// 盘点（T1 Step 0）：`direct` 关 / 串尾（逃生口）/ 连续模式三列界面前 5，另列连续模式的
/// 引擎序前 6（`文字/消费数`）。`-- --ignored --nocapture`，结果见设计 §5.4 的盘点表。
#[test]
#[ignore]
fn inventory_tail_vs_continuous() {
    if !has_data() {
        return;
    }
    let (tail, cont, off) = (coord_tail(), coord(true), coord(false));
    let settings = cont.engine_mgr.aux_code_settings_cached();
    let rt = cont.ensure_aux_code_runtime(&settings.sources);
    let active = cont.engine_mgr.active_schema_id();
    for input in [
        "uidu", "uidup", "uidupl", "uiduy", "uipdu", "goqk", "goqkk", "goqkg", "goqkgd", "goqkx",
        "gokqk", "xl", "xlr", "xlrn", "jxl", "jxlm", "uip", "tuuugr", "tuuugrk", "tuuugrkd",
        "nihc", "nihcnz", "nirxhc", "qiddn", "qiddnb",
    ] {
        let lk = Arc::new(rt.lookup(&cont.engine_mgr));
        let r = cont.engine_mgr.convert_with_opts(
            &active,
            input,
            50,
            super::continuous_opts(&lk, None),
        );
        let eng: Vec<String> = r
            .candidates
            .iter()
            .take(6)
            .map(|c| format!("{}/{}", c.text, c.consumed_length))
            .collect();
        eprintln!(
            "INV\t{input}\toff {:?}\ttail {:?}\tcont {:?}\tengine {:?}",
            top5(&after(&off, input)),
            top5(&after(&tail, input)),
            top5(&after(&cont, input)),
            eng
        );
    }
}

/// 单键耗时：直接辅助开 / 关各跑一遍同一批输入（3 条整句 + §11 示例逐键敲入），
/// 取每键平均。`cargo test … direct_aux_perf -- --ignored --nocapture`，≥ 3 次取中位。
#[test]
#[ignore]
fn direct_aux_perf() {
    if !has_data() {
        return;
    }
    let inputs = [
        "wojbtmhfgcxk",
        "tamfvgzdkdhv",
        "mktmtmqihfhc",
        "uidupl",
        "goqkgd",
        "xlrn",
        "jxlm",
    ];
    let (on, off) = (coord(true), coord(false));
    // 预热：词库 mmap、码表懒加载、各种缓存。
    for c in [&on, &off] {
        for s in inputs {
            type_each(c, s, |_| {});
        }
    }
    let rounds = 20;
    // 分两组报：长句（单音节前缀的兜底解码每句只摊一次）与短词（每个词都要摊一次）。
    let groups: [(&str, &[&str]); 3] = [
        ("全部", &inputs),
        ("整句", &inputs[..3]),
        ("短词", &inputs[3..]),
    ];
    for (group, list) in groups {
        for (name, c) in [("off", &off), ("on", &on), ("off", &off), ("on", &on)] {
            let mut keys = 0u32;
            let t = std::time::Instant::now();
            for _ in 0..rounds {
                for s in list {
                    type_each(c, s, |_| keys += 1);
                }
            }
            let per = t.elapsed() / keys;
            eprintln!("direct {name} [{group}]: {keys} 键，平均 {per:?}/键");
        }
    }
}
