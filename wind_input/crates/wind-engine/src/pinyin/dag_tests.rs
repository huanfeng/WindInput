use super::*;
use crate::pinyin::syllable::SyllableTrie;

fn graph(input: &str) -> SegGraph {
    SegGraph::from_dag(&Dag::build(input, &SyllableTrie::new()))
}

/// 模糊拼写层让「本身不成音节的错音串」切得动。
///
/// 这是模糊音能被执行的**前提**：变体展开发生在切分之后，切不出 `tin|zhi` 就等于
/// 整条模糊链路一次都没跑 —— 用户看到的是「开了 in-ing，`tinzhi` 仍然打不出停止」。
#[test]
fn fuzzy_spelling_layer_makes_mistyped_code_segmentable() {
    use crate::pinyin::fuzzy::{FuzzyConfig, fuzzy_spellings};

    let mut trie = SyllableTrie::new();
    let plain = Dag::build("tinzhi", &trie);
    assert_eq!(
        plain.maximum_match(),
        vec!["ti".to_string()],
        "修复前的形态"
    );
    assert_eq!(plain.unmatched_tail(), "nzhi");

    trie.load_fuzzy_spellings(&fuzzy_spellings(&FuzzyConfig {
        in_ing: true,
        ..Default::default()
    }));
    let dag = Dag::build("tinzhi", &trie);
    assert_eq!(
        dag.maximum_match(),
        vec!["tin".to_string(), "zhi".to_string()]
    );
    assert_eq!(dag.unmatched_tail(), "", "整串须被覆盖");
    // 整句路径同样要走得通：词图上 0..6 必须存在一条路径，否则 lattice 的模糊分支
    // 会在 `graph.any_path` 处直接 continue。
    assert!(
        SegGraph::from_dag(&dag).any_path(0, 6, 8).is_some(),
        "词图 0..6 须可达"
    );
    // 真值推导仍走严格切分，不受影响。
    assert_eq!(
        Dag::build_strict("tinzhi", &trie).maximum_match(),
        vec!["ti".to_string()]
    );
}

/// 模糊拼写层**只在原本切不动的地方添边**：输入本身已是合法音节序列时，
/// 从起点可达的那部分切分图逐位不变。
///
/// 这条不变量把「模糊音开了之后候选变了」拆成两件事：切分变了（本改动的责任）与
/// 变体展开带来了新候选（模糊音本来的效果）。没有它，任何一次排序波动都会被归到
/// 切分头上 —— 实测 `xian` 开全组后「西安」从第 3 退到第 6，正是后者（`ian_iang`
/// 让「香/想/相」进来竞争），与切分无关。
///
/// ⚠️ **只断言可达位置**：不可达位置上确实会多出边，`zhongguo` 的位置 1 就多了一条
/// `ho`（f_h 组，`ho`→`fo`）。它构不成任何从 0 起的切分，`LatticeBuilder::build` 也
/// 有 `require_reachable` 守卫把它挡在外面。把断言写成「所有位置」会把这类无害差异
/// 报成回归。
#[test]
fn fuzzy_spelling_layer_does_not_disturb_valid_input() {
    use crate::pinyin::fuzzy::{FuzzyConfig, fuzzy_spellings};

    let mut trie = SyllableTrie::new();
    trie.load_fuzzy_spellings(&fuzzy_spellings(&FuzzyConfig {
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
    }));

    for input in [
        "nihao",
        "zhongguo",
        "beijing",
        "xian",
        "fangan",
        "jisuanji",
        "shengchan",
        "guanli",
        // eng_ong 的零声母端 `eng`→`ong`：`ong` 若成了可切的边，`xi|ong` 就是一种
        // 新切分（jiong/qiong 同形）。
        "xiongdi",
        "jiongpo",
        "qiongren",
        // un_ong：`jun`/`qun`/`xun` 的对端 `jong`/`qong`/`xong` 不是音节。
        "junren",
        "xunzhao",
    ] {
        let fuzzy = Dag::build(input, &trie);
        let strict = Dag::build_strict(input, &trie);
        assert_eq!(
            fuzzy.maximum_match(),
            strict.maximum_match(),
            "{input} 的最大匹配不该变"
        );
        assert_eq!(
            fuzzy.unmatched_tail(),
            strict.unmatched_tail(),
            "{input} 的残码不该变"
        );
        // 可达位置的出边须逐位相同——多一条边就多一种切分解释，会一路影响到词图与整句。
        let (gf, gs) = (SegGraph::from_dag(&fuzzy), SegGraph::from_dag(&strict));
        for p in 0..input.len() {
            assert_eq!(
                gf.is_reachable(p),
                gs.is_reachable(p),
                "{input} 在位置 {p} 的可达性不该变"
            );
            if !gs.is_reachable(p) {
                continue; // 见上文：不可达位置上的多余边不会被消费
            }
            assert_eq!(
                gf.ends_within(p, 8),
                gs.ends_within(p, 8),
                "{input} 在位置 {p} 的出边不该变"
            );
        }
    }
}

/// 定长枚举必须给出**全部**同音节数的切分，不能只给最大匹配那一条。
///
/// `angan` 上 `an|gan` 与 `ang|an` 都是合法的 2 音节切分——正是这种歧义让
/// `maximum_match` 无从取舍，也正是字数约束之外还需要读音消歧的原因。
#[test]
fn enumerates_every_split_of_the_same_length() {
    let mut ps = graph("angan").paths_with_edges(0, 5, 2, 16);
    ps.sort();
    assert_eq!(ps, vec![vec![0, 2], vec![0, 3]], "an|gan 与 ang|an");
}

/// 字数约束的核心作用：同一个码，不同音节数各自得唯一解。
#[test]
fn constraint_selects_by_syllable_count() {
    let g = graph("xian");
    assert_eq!(g.paths_with_edges(0, 4, 2, 16), vec![vec![0, 2]], "xi|an");
    assert_eq!(g.paths_with_edges(0, 4, 1, 16), vec![vec![0]], "xian");
    // `xianning` 的 3 音节解唯一——2 音节的 xian|ning 被约束排除在外。
    let g2 = graph("xianning");
    assert_eq!(g2.paths_with_edges(0, 8, 3, 16), vec![vec![0, 2, 4]]);
    assert_eq!(g2.paths_with_edges(0, 8, 2, 16), vec![vec![0, 4]]);
}

/// 无解与边界情形：切不出的码、放不下的边数、零边。
#[test]
fn rejects_impossible_constraints() {
    assert!(graph("wgkq").paths_with_edges(0, 4, 1, 16).is_empty());
    // 每条边至少 1 字节 ⇒ 4 字节放不下 5 条边
    assert!(graph("xian").paths_with_edges(0, 4, 5, 16).is_empty());
    // n == 0 只在 p == q 时有一条空路径
    let empty: Vec<Vec<usize>> = vec![Vec::new()];
    assert_eq!(graph("xian").paths_with_edges(0, 0, 0, 16), empty);
    assert!(graph("xian").paths_with_edges(0, 4, 0, 16).is_empty());
}

/// `limit` 必须真的封顶——调用方据此判「多解」，截断不可静默当成唯一解。
#[test]
fn limit_caps_results() {
    assert_eq!(graph("angan").paths_with_edges(0, 5, 2, 1).len(), 1);
}
