//! 用户/临时词的 `boundary == 0` 不等于「音节数算不出」，两条产出通道都不得据此把它当
//! 「无信息」处置。
//!
//! 手输码用户词（`dict import`、只填码不填音节、旧版数据）恒 `boundary = 0`，但码本身是合法
//! 全拼串，DAG 现切即得音节数。召回门、step4 显示序档位、`should_promote_user_completion`、
//! step 6.7 全拼降级支路的档位回填都须走 `effective_boundary`，否则同一个词带不带边界字段
//! 就决定了它能不能被看见。
//!
//! ## 「距词尾太远所以不上浮」只能在 step 6.7 支路上验
//!
//! step 6.3 的 `retain` 用 `syllable_cap = started + max_extra` 裁掉 `is_prefix` 候选，与判据里的
//! `remaining <= max_extra` 是同一个不等式，主路径上的下侧对照必然空过（判据改成 `return true`
//! 也全绿）。故下侧对照挂在 6.7 支路，那条路的用户词刻意不受 `completion_syllable_cap` 约束
//! （见 `recall_full_pinyin` 里的取舍说明）。
//!
//! ## 夹具的要害
//!
//! 每组两条用户词同码、同权重、只差 boundary（文本加「甲/乙」区分，否则 step 6 的同文合并会把
//! 两条并成一条）。判据是两条必须表现一致，而不是某条落在某个具体位次（会随夹具规模漂移）。
//!
//! 自带 wdat 夹具 + `data/schemas/shuangpin`（版本控制内），不依赖 `build_dev/data`。

use std::sync::Arc;
use wind_dict::cached::CachedDict;
use wind_dict::datformat::WdatWriter;
use wind_engine::Engine;
use wind_engine::pinyin::shuangpin::{Layout, ShuangpinConverter};
use wind_engine::pinyin::{Config as PyConfig, PinyinEngine};
use wind_store::Store;

const CODE: &str = "qingfengshurufa";
const SYLS: &[&str] = &["qing", "feng", "shu", "ru", "fa"];
const BOUNDED: &str = "清风输入法甲";
const NO_BOUNDARY: &str = "清风输入法乙";
/// 只用于 [`merge_branch_user_weight_applies_without_boundary`]：系统词库与用户词**同文**，
/// 走 step 6 的同文合并分支（与上面两条走的「新增」分支是不同的两段代码）。
const MERGED: &str = "清风输入法丙";

/// `SYLS` 拼成的扁平码 + 音节起点位图。返回的码必须与 [`CODE`] 一致（自检）。
fn code_and_boundary() -> (String, u64) {
    let mut code = String::new();
    let mut b: u64 = 0;
    for s in SYLS {
        b |= 1u64 << code.len();
        code.push_str(s);
    }
    assert_eq!(code, CODE, "夹具自检：音节拼接结果必须等于用户词的码");
    (code, b)
}

/// 最小系统词库：只放前 1-2 音节的词，让系统侧产出几条竞争候选，但不含本用例的目标词。
fn sys_dict(tag: &str) -> CachedDict {
    let dir = std::env::temp_dir().join(format!("wind_uw_nb_{tag}"));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let wdat = dir.join("t.wdat");
    let mut w = WdatWriter::new();
    w.add_with_boundary("qing".into(), vec![("请".into(), 90000, 0, 0b1)]);
    w.add_with_boundary("qingfeng".into(), vec![("清风".into(), 8000, 0, 0b10001)]);
    w.write(&wdat).unwrap();
    CachedDict::load_at(&dir.join("t.dict.yaml"), &wdat).expect("加载 wdat 夹具")
}

/// 装好两条只差 boundary 的用户词的引擎。`shuangpin = true` 时开双拼 + 允许全拼输入，
/// 把产出通道切到 step 6.7 降级支路。
fn engine(tag: &str, max_extra: u32, shuangpin: bool) -> PinyinEngine {
    let p = std::env::temp_dir().join(format!("wind_uw_nb_{tag}.redb"));
    let _ = std::fs::remove_file(&p);
    let s = Arc::new(Store::open(&p).expect("打开 store"));
    let (code, boundary) = code_and_boundary();
    s.add_user_word("pinyin", &code, BOUNDED, 5000, boundary)
        .expect("写入有边界用户词");
    s.add_user_word("pinyin", &code, NO_BOUNDARY, 5000, 0)
        .expect("写入无边界用户词");

    let dm = wind_dict::manager::DictManager::new();
    dm.register_layer(Box::new(wind_store::StoreUserLayer::new(
        s.clone(),
        "pinyin",
    )));
    dm.register_layer(Box::new(wind_store::StoreTempLayer::new(
        s.clone(),
        "pinyin",
    )));

    let cfg = PyConfig {
        allow_full_pinyin: shuangpin,
        completion_min_syllables: 1,
        completion_max_extra_syllables: max_extra,
        ..Default::default()
    };
    let e = PinyinEngine::new(cfg, sys_dict(tag)).with_store_layers(Arc::new(dm));
    if shuangpin {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../../data/schemas/shuangpin");
        let layout = Layout::from_toml(&dir.join("xiaohe.toml")).expect("加载小鹤布局");
        e.with_shuangpin(ShuangpinConverter::new(layout))
    } else {
        e
    }
}

/// 一条候选上被本文件关注的几项。
///
/// `pos` 不参与比较（见 [`cmp_key`]）：两条对照词同码同权重，相对先后由插入序决定，
/// 拿它断言等于钉住稳定排序的实现细节。留着只为失败信息里能看出各自落在哪。
#[derive(Debug)]
struct Seen {
    /// 只出现在 `Debug` 输出里（失败信息），没有任何断言读它 —— `derive(Debug)`
    /// 不被 `dead_code` 视作「读」，故显式放行。
    #[allow(dead_code)]
    pos: usize,
    extra: u8,
    promoted: bool,
}

/// 参与比较的投影。写成函数而非给 `Seen` 手写 `PartialEq`，断言比的是什么一目了然，
/// `pos` 留在附带的 `{a:?} {b:?}` 里当线索。
fn cmp_key(s: &Seen) -> (u8, bool) {
    (s.extra, s.promoted)
}

/// 取两条对照词的表现。**两条都必须在候选里** —— 少了任何一条，「对照」就不成立，
/// 断言会变成空过（本仓已有多次「护栏因前提不成立而恒绿」的前科）。
fn both(e: &PinyinEngine, input: &str) -> (Seen, Seen) {
    let r = e.convert(input, 50).expect("convert 成功");
    let look = |t: &str| -> Seen {
        let pos = r
            .candidates
            .iter()
            .position(|c| c.text == t)
            .unwrap_or_else(|| {
                panic!(
                    "前提不成立：`{input}` 下未召回「{t}」，共 {} 条候选 {:?}",
                    r.candidates.len(),
                    r.candidates.iter().map(|c| &c.text).collect::<Vec<_>>()
                )
            });
        let c = &r.candidates[pos];
        assert!(
            c.is_prefix,
            "前提不成立：「{t}」在 `{input}` 下应是前缀补全（本文件验的是补全档位），\
             实际 is_prefix=false"
        );
        Seen {
            pos,
            extra: c.completion_extra_syllables,
            promoted: c.is_promoted_completion,
        }
    };
    (look(BOUNDED), look(NO_BOUNDARY))
}

/// 主路径 step 6：手输码用户词的上浮判据必须与带边界的同码词一致。
///
/// `qingf` = qing + 残码 f ⇒ started 2，词 5 音节 ⇒ 距词尾 3 ≤ max_extra 5 ⇒ 应上浮；
/// 无边界那条若退化到 `started >= 3` 门槛就不上浮。
#[test]
fn main_path_no_boundary_word_promotes_like_bounded_one() {
    let e = engine("main", 5, false);
    let (a, b) = both(&e, "qingf");
    assert!(
        a.promoted,
        "前提不成立：带边界那条本身就该上浮（started 2、距词尾 3 ≤ 5），实际 {a:?}"
    );
    assert_eq!(
        cmp_key(&a),
        cmp_key(&b),
        "同码同音节数、只差 boundary 的两条，(extra, promoted) 必须一致：\
         有边界 {a:?}，无边界 {b:?}"
    );
    assert_eq!(b.extra, 3, "5 音节词在 started=2 的输入下 extra 须为 3");
}

/// 下侧对照：距词尾超出 `max_extra` 时两条都不上浮。全仓唯一能让这个命题可观测的位置
/// （理由见文件头），少了它，判据改成 `return true` 也能全绿。
///
/// `max_extra = 1`、`qingfengs` = qing|feng + 残码 s ⇒ started 3，词 5 音节 ⇒ 距词尾 2 > 1。
#[test]
fn fallback_branch_far_word_promotes_for_neither() {
    let e = engine("fb_far", 1, true);
    let (a, b) = both(&e, "qingfengs");
    assert!(
        !a.promoted,
        "带边界：距词尾 2 > max_extra 1，不该上浮，实际 {a:?}"
    );
    assert_eq!(
        cmp_key(&a),
        cmp_key(&b),
        "不上浮这一侧同样不该因「填没填 boundary」而不同：有边界 {a:?}，无边界 {b:?}"
    );
    // 档位照算（与上浮是两件事）：5 音节词在 started=3 下 extra = 2。
    assert_eq!(a.extra, 2, "不上浮不影响显示序档位");
}

/// step 6.7 全拼降级支路：同样不得因 `boundary == 0` 跳过档位回填与上浮判据。
///
/// 双拼方案下打全拼 `qingfengs`，这条支路是该用户词的唯一产出通道（主路径把击键当双拼读）。
/// 无边界那条若被跳过，`extra` 恒 0（真值 2）且 `promoted=false`。
#[test]
fn fallback_branch_no_boundary_word_gets_tier_and_promotion() {
    let e = engine("fb", 5, true);
    let (a, b) = both(&e, "qingfengs");
    assert!(
        a.promoted,
        "前提不成立：带边界那条在降级支路本身就该上浮，实际 {a:?}"
    );
    assert_eq!(
        cmp_key(&a),
        cmp_key(&b),
        "降级支路里同码同音节数、只差 boundary 的两条，(extra, promoted) 必须一致：\
         有边界 {a:?}，无边界 {b:?}"
    );
    // started = qing|feng + 残码 s = 3，词 5 音节 ⇒ extra = 2。
    assert_eq!(b.extra, 2, "5 音节词在 started=3 的输入下 extra 须为 2");
}

/// step 6 同文合并分支：用户词与系统词库同文时，用户权重能否生效同样不看 boundary。
///
/// 与「新增」分支判据同源但代码另写一段（`existing.weight = promotion_cap.map_or(...)`）。
/// 合并分支控制的是用户权重生不生效：判据为真 ⇒ `max(系统, 用户)` 再受 `promotion_cap` 封顶；
/// 为假 ⇒ 用户配的权重白配。故断言钉在 `weight` 上。
#[test]
fn merge_branch_user_weight_applies_without_boundary() {
    const SYS_WEIGHT: i32 = 1;
    const USER_WEIGHT: i32 = 5000;

    // 系统词库里放一条同文词（`boundary = 0`，模拟导入的扩展词库），用户词同码同文。
    let dir = std::env::temp_dir().join("wind_uw_nb_merge_dict");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let wdat = dir.join("t.wdat");
    let mut w = WdatWriter::new();
    w.add_with_boundary("qing".into(), vec![("请".into(), 90000, 0, 0b1)]);
    w.add_with_boundary(CODE.into(), vec![(MERGED.into(), SYS_WEIGHT, 0, 0)]);
    w.write(&wdat).unwrap();
    let sys = CachedDict::load_at(&dir.join("t.dict.yaml"), &wdat).expect("加载 wdat 夹具");

    let p = std::env::temp_dir().join("wind_uw_nb_merge.redb");
    let _ = std::fs::remove_file(&p);
    let store = Arc::new(Store::open(&p).expect("打开 store"));
    store
        .add_user_word("pinyin", CODE, MERGED, USER_WEIGHT, 0)
        .expect("写入无边界用户词");
    let dm = wind_dict::manager::DictManager::new();
    dm.register_layer(Box::new(wind_store::StoreUserLayer::new(
        store.clone(),
        "pinyin",
    )));
    dm.register_layer(Box::new(wind_store::StoreTempLayer::new(store, "pinyin")));
    let cfg = PyConfig {
        completion_min_syllables: 1,
        completion_max_extra_syllables: 5,
        ..Default::default()
    };
    let e = PinyinEngine::new(cfg, sys).with_store_layers(Arc::new(dm));

    let r = e.convert("qingf", 50).expect("convert 成功");
    let c = r
        .candidates
        .iter()
        .find(|c| c.text == MERGED)
        .unwrap_or_else(|| {
            panic!(
                "前提不成立：同文词没进候选，实际 {:?}",
                r.candidates.iter().map(|c| &c.text).collect::<Vec<_>>()
            )
        });
    // 前提：它确实走了合并分支（系统侧产出的前缀补全被用户层合并，而非用户层新增）。
    assert!(
        c.is_prefix,
        "前提不成立：该候选应是系统侧的前缀补全（合并分支只对 is_prefix 生效）"
    );
    assert!(
        c.meta.is_user_dict,
        "前提不成立：合并没发生（用户层来源标记没置上），本用例测的就不是合并分支"
    );

    assert!(
        c.weight >= USER_WEIGHT,
        "started 2、距词尾 3 ≤ max_extra 5 ⇒ 用户权重应生效（≥{USER_WEIGHT}），\
         实际 {} —— 停在系统权重 {SYS_WEIGHT} 说明判据把 boundary=0 当成了「算不出」",
        c.weight
    );
}
