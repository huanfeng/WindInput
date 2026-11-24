use super::*;
use crate::table::AuxCodeTable;
use wind_candidate::CandidateSource;

/// 构造最简候选（只填过滤用到的 text/source，其余用默认值）
fn cand(text: &str) -> Candidate {
    Candidate {
        text: text.into(),
        source: CandidateSource::Pinyin,
        ..Default::default()
    }
}

fn sample_table() -> AuxCodeTable {
    AuxCodeTable::from_rows(vec![
        ('李', "mz"), // 木+子
        ('樱', "my"), // 木+婴
        ('林', "mm"), // 木+木
        ('河', "sk"), // 氵+可
        ('花', "ch"), // 艹+化
        ('草', "cz"), // 艹+早
    ])
}

fn session() -> AuxCodeSession {
    AuxCodeSession::new(vec![cand("李"), cand("樱"), cand("河"), cand("花")])
}

/// 缓冲 push/pop / is_empty / buffer 访问器
#[test]
fn buffer_ops() {
    let mut s = AuxCodeSession::new(Vec::new());
    assert!(s.is_empty());
    s.push_char('m');
    s.push_char('z');
    assert_eq!(s.buffer(), "mz");
    assert!(!s.is_empty());
    assert_eq!(s.pop_char(), Some('z'));
    assert_eq!(s.pop_char(), Some('m'));
    assert_eq!(s.pop_char(), None);
    assert!(s.is_empty());
}

/// apply：只返回通过筛选的候选（命中子序列），被滤候选**不进列表**
#[test]
fn apply_keeps_only_matching() {
    let t = sample_table();
    let mut s = session();
    s.push_char('m');
    let list = s.apply(&t, &AuxCodeFilterOptions::default());
    let texts: Vec<_> = list.iter().map(|c| c.text.as_str()).collect();
    assert_eq!(
        texts,
        vec!["李", "樱"],
        "m：命中子序列；河/花 被滤，不再出现在列表里"
    );
}

/// apply 每次从快照重筛（不叠加）：退格还原语义，而不是在已筛选列表上再筛
#[test]
fn apply_always_reesifts_from_snapshot() {
    let t = sample_table();
    let mut s = session();
    s.push_char('m');
    let first = s.apply(&t, &AuxCodeFilterOptions::default());
    assert_eq!(first.len(), 2);
    // 缓冲没变时反复重筛结果一致；缓冲清空 = 从快照原样放行
    s.pop_char();
    let cleared = s.apply(&t, &AuxCodeFilterOptions::default());
    assert_eq!(cleared.len(), 4, "空缓冲 passthrough：原样放行全部候选");
}

/// restore_original：还原快照（退出语义）
#[test]
fn restore_original_unmarks_snapshot() {
    let t = sample_table();
    let mut s = session();
    s.push_char('m');
    let applied = s.apply(&t, &AuxCodeFilterOptions::default());
    assert_eq!(applied.len(), 2, "apply 只返回命中者");
    let restored = s.restore_original();
    assert_eq!(
        restored.iter().map(|c| c.text.as_str()).collect::<Vec<_>>(),
        vec!["李", "樱", "河", "花"],
        "还原 = 快照原样顺序"
    );
}
