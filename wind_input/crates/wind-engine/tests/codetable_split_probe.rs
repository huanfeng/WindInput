//! 逆切分（`split_input`）在**真实码表词库**上的编码空间探针。
//!
//! # 它要回答什么
//!
//! 论坛 t11 的收益论证是**在小鹤音形上**做的：二简空间 26×26=676，声韵组合只占约 400，
//! 余下的码位放简词，于是四码空码位大量存在、切出来的多半是「二简词 + 二简字」。
//! 这套论证换一张码表未必成立，而功能是给**所有**码表方案开放的开关。
//!
//! 本探针把三个数落到实处，对任何一张码表都可复算：
//!
//! | 指标 | 含义 | 为什么要看 |
//! |---|---|---|
//! | 四码空码率 | 合法四码串里查不到任何词条的比例 | 太低 ⇒ 功能没机会触发 |
//! | 可切分率 | 空码串中前后两段**都**查得到的比例 | 这才是功能的实际覆盖面 |
//! | 后段重码分布 | 可切分串的后段候选数直方图 | 后段唯一 = 能自动上屏；这是收益论证的核心 |
//!
//! ```text
//! cargo test -p wind-engine --test codetable_split_probe -- --ignored --nocapture
//! ```
//!
//! # ⚠️ 这不是「准确率」
//!
//! 这里量的是**编码空间的形状**，不是切出来的词对不对——后者要人读才知道，故本探针
//! 只抽样打印实例供人工判读，不给任何自动判分。拿词库自身回测切分正确率是设计文档
//! §7 点过名的假绿。
//!
//! # 换一张码表怎么跑
//!
//! 改 [`PROBE_DICTS`] 里的路径即可（相对 `build_dev/data/schemas`）。音形方案自带的
//! `.dict.yaml` 放进去就能出同一份报告，与五笔的数直接对照。
//!
//! # 词库不在时静默跳过
//!
//! 词库是构建产物（`build_dev/`，不入库）。找不到就打印一行说明并返回——
//! 这条探针本就 `#[ignore]`，不该因为环境没构建过而红。

use std::path::PathBuf;
use std::sync::Arc;

use wind_dict::cached::CachedDict;
use wind_dict::{DictManager, SystemDictLayer};

/// 要体检的码表：`(展示名, 相对 build_dev/data/schemas 的 .dict.yaml 路径, 码元字符集, 码长)`。
///
/// 五笔码元是 `a-y`（`z` 是万能键、不入码），故枚举空间是 25⁴ 而非 26⁴。
/// 加别的码表时照填它自己的码元集——枚举空间填错会让空码率整体失真。
const PROBE_DICTS: &[(&str, &str, &str, usize)] = &[(
    "五笔86（极点主库）",
    "wubi86/wubi86_jidian.dict.yaml",
    "abcdefghijklmnopqrstuvwxy",
    4,
)];

fn schemas_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../build_dev/data/schemas")
}

fn load(rel: &str) -> Option<Arc<DictManager>> {
    let yaml = schemas_dir().join(rel);
    if !yaml.exists() {
        return None;
    }
    let wdat = yaml.with_extension("wdat");
    let cached = CachedDict::load_at_with(&yaml, &wdat, false).ok()?;
    let dm = DictManager::new();
    dm.register_layer(Box::new(SystemDictLayer::new(cached, "probe")));
    Some(Arc::new(dm))
}

/// 一张码表的体检报告。
struct Report {
    /// 枚举的合法码串总数。
    total: usize,
    /// 查不到任何词条的（= 逆切分默认档的触发面）。
    empty: usize,
    /// 空码中前后两段都查得到的（= 实际覆盖面）。
    splittable: usize,
    /// 可切分串中后段**唯一**的（= 能自动上屏的那部分）。
    back_unique: usize,
    /// 后段候选数直方图，下标即候选数（0 不计），末槽是「≥8」。
    back_hist: [usize; 9],
    /// 抽样实例：`(码串, 前段首选, 后段候选文本)`。
    samples: Vec<(String, String, Vec<String>)>,
}

fn probe(dm: &DictManager, chars: &str, code_len: usize, sample_every: usize) -> Report {
    assert!(
        code_len >= 4 && code_len.is_multiple_of(2),
        "切点需偶数码长"
    );
    let half = code_len / 2;
    let alphabet: Vec<char> = chars.chars().collect();
    let n = alphabet.len();

    // 先把所有「半码段」的候选查好——枚举四码时每段会被重复问 n² 次，
    // 预查一次把 25⁴ 次词典查询压到 25²。
    let mut seg_cands: std::collections::HashMap<String, Vec<String>> =
        std::collections::HashMap::new();
    let mut seg_codes: Vec<String> = Vec::with_capacity(n.pow(half as u32));
    // half=2 时相当于两重循环，这里写成通用的逐位展开，换码长不必改。
    let mut stack: Vec<String> = vec![String::new()];
    for _ in 0..half {
        let mut next = Vec::with_capacity(stack.len() * n);
        for s in &stack {
            for &c in &alphabet {
                let mut t = s.clone();
                t.push(c);
                next.push(t);
            }
        }
        stack = next;
    }
    for code in stack {
        let texts: Vec<String> = dm
            .search(&code, 16)
            .into_iter()
            .map(|c| c.text)
            .collect::<Vec<_>>();
        if !texts.is_empty() {
            seg_cands.insert(code.clone(), texts);
        }
        seg_codes.push(code);
    }

    let mut r = Report {
        total: 0,
        empty: 0,
        splittable: 0,
        back_unique: 0,
        back_hist: [0; 9],
        samples: Vec::new(),
    };

    for front in &seg_codes {
        for back in &seg_codes {
            let full = format!("{front}{back}");
            r.total += 1;
            // 码长等于方案满码长时不存在更长后继（本探针的码表都是定长四码），
            // 故「候选窗全空」等价于精确查询为空。
            if !dm.search(&full, 1).is_empty() {
                continue;
            }
            r.empty += 1;
            let (Some(fc), Some(bc)) = (seg_cands.get(front), seg_cands.get(back)) else {
                continue;
            };
            r.splittable += 1;
            let k = bc.len().min(8);
            r.back_hist[k] += 1;
            if bc.len() == 1 {
                r.back_unique += 1;
            }
            if r.splittable.is_multiple_of(sample_every) && r.samples.len() < 25 {
                r.samples.push((
                    full,
                    fc[0].clone(),
                    bc.iter().take(4).cloned().collect::<Vec<_>>(),
                ));
            }
        }
    }
    r
}

#[test]
#[ignore = "需要 build_dev 下的真实词库（构建产物，不入库）"]
fn split_encoding_space_probe() {
    let mut ran = false;
    for (name, rel, chars, code_len) in PROBE_DICTS {
        let Some(dm) = load(rel) else {
            println!("· 跳过 {name}：找不到 {rel}（需先构建一次 build_dev）");
            continue;
        };
        ran = true;
        let r = probe(&dm, chars, *code_len, 977);

        let pct = |x: usize, base: usize| {
            if base == 0 {
                0.0
            } else {
                x as f64 * 100.0 / base as f64
            }
        };
        println!("\n═══ {name} ═══");
        println!("枚举 {} 码串（码元 {} 个）", r.total, chars.chars().count());
        println!(
            "  空码        {:>7}  ({:5.2}% 的码空间)   ← 逆切分默认档的触发面",
            r.empty,
            pct(r.empty, r.total)
        );
        println!(
            "  其中可切分  {:>7}  ({:5.2}% 的空码)     ← 实际覆盖面",
            r.splittable,
            pct(r.splittable, r.empty)
        );
        println!(
            "  后段唯一    {:>7}  ({:5.2}% 的可切分)   ← 这部分能四码自动上屏",
            r.back_unique,
            pct(r.back_unique, r.splittable)
        );
        print!("  后段重码分布 ");
        for (k, v) in r.back_hist.iter().enumerate().skip(1) {
            let label = if k == 8 {
                "≥8".to_string()
            } else {
                k.to_string()
            };
            print!("{label}:{v} ");
        }
        println!();
        println!("  抽样（前段首选 + 后段前若干候选）：");
        for (code, f, b) in &r.samples {
            let half = *code_len / 2;
            println!(
                "    {}'{}  →  {} + [{}]",
                &code[..half],
                &code[half..],
                f,
                b.join(" ")
            );
        }

        // 探针不判分，只挡住「一条都没查到」这种环境/路径出错的情形——
        // 那时上面所有百分比都是 0，不设防会被当成「这张码表不适合」的结论。
        assert!(
            r.splittable > 0,
            "{name}：一条可切分的码串都没有，多半是词库路径或码元集填错了"
        );
    }
    if !ran {
        println!("\n（没有可用词库，探针未产出数据）");
    }
}
