use super::*;
use std::sync::atomic::{AtomicUsize, Ordering};
use wind_dict::codetable::CodetableDict;

fn mem_source(
    id: &str,
    entries: &[(&str, &str, i32)],
    calls: Arc<AtomicUsize>,
) -> DisabledDictSource {
    let owned: Vec<(String, String, i32)> = entries
        .iter()
        .map(|(c, t, w)| (c.to_string(), t.to_string(), *w))
        .collect();
    DisabledDictSource {
        id: id.into(),
        base_order: 3,
        default_weight: None,
        load: Arc::new(move || {
            calls.fetch_add(1, Ordering::SeqCst);
            let mut d = CodetableDict::empty();
            for (i, (c, t, w)) in owned.iter().enumerate() {
                d.merge_single(c.clone(), t.clone(), *w, i as i32);
            }
            Ok(CachedDict::Memory(d))
        }),
    }
}

fn slot(p: &str) -> String {
    p.replace('?', &wind_dict::WILDCARD_SLOT.to_string())
}

/// spec §4.2：首次查询才加载，之后复用；启用集里的库不进影子。
#[test]
fn loads_lazily_once_and_skips_enabled() {
    let calls = Arc::new(AtomicUsize::new(0));
    let d = DisabledDictLayers::new(
        vec![
            mem_source("xz", &[("uuia", "门头沟区", 1)], calls.clone()),
            mem_source("ext", &[("uuib", "已启用库", 1)], calls.clone()),
        ],
        ["ext".to_string()],
        None,
    );
    assert_eq!(d.load_count(), 0, "构造不加载");
    let texts = |v: Vec<Candidate>| v.into_iter().map(|c| c.text).collect::<Vec<_>>();
    assert_eq!(
        texts(d.search_pattern(&slot("uui?"), wind_dict::WILDCARD_SLOT, 10, true)),
        ["门头沟区"]
    );
    d.search_pattern(&slot("uui?"), wind_dict::WILDCARD_SLOT, 10, true);
    assert_eq!(d.load_count(), 1, "第二次复用");
    assert_eq!(
        calls.load(Ordering::SeqCst),
        1,
        "只读了影子集合里的那一个库"
    );
}

/// 预热建一遍、之后查询复用；已建好时 `warm` 不再建，`needs_warm` 转 false。
#[test]
fn warm_builds_once_and_query_reuses_it() {
    let calls = Arc::new(AtomicUsize::new(0));
    let d = DisabledDictLayers::new(
        vec![mem_source("xz", &[("uuia", "门头沟区", 1)], calls.clone())],
        std::iter::empty(),
        None,
    );
    assert!(d.needs_warm());
    assert!(d.warm(), "首次预热真的建了");
    assert!(!d.needs_warm());
    assert!(!d.warm(), "已建好不再建");
    d.search_pattern(&slot("uui?"), wind_dict::WILDCARD_SLOT, 10, true);
    assert_eq!(d.load_count(), 1);
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    d.mark_disabled("xz");
    assert!(d.needs_warm(), "作废后需要重新预热");
}

/// 禁用一个库 ⇒ 它进影子集合、已加载的作废重建。
#[test]
fn mark_disabled_extends_set_and_drops_loaded() {
    let calls = Arc::new(AtomicUsize::new(0));
    let d = DisabledDictLayers::new(
        vec![
            mem_source("xz", &[("uuia", "门头沟区", 1)], calls.clone()),
            mem_source("ext", &[("uuib", "已启用库", 1)], calls.clone()),
        ],
        ["ext".to_string()],
        None,
    );
    d.search_pattern(&slot("uui?"), wind_dict::WILDCARD_SLOT, 10, true);
    d.mark_disabled("ext");
    let got: Vec<String> = d
        .search_pattern(&slot("uui?"), wind_dict::WILDCARD_SLOT, 10, true)
        .into_iter()
        .map(|c| c.text)
        .collect();
    assert!(
        got.contains(&"已启用库".to_string()) && got.contains(&"门头沟区".to_string()),
        "{got:?}"
    );
    assert_eq!(d.load_count(), 2);
}

/// 文件缺失 / 解析失败：跳过该库、warn，不影响其余库，也不 panic。
#[test]
fn failing_source_is_skipped() {
    let calls = Arc::new(AtomicUsize::new(0));
    let broken = DisabledDictSource {
        id: "gone".into(),
        base_order: 3,
        default_weight: None,
        load: Arc::new(|| anyhow::bail!("no such file")),
    };
    let d = DisabledDictLayers::new(
        vec![broken, mem_source("xz", &[("uuia", "门头沟区", 1)], calls)],
        std::iter::empty(),
        None,
    );
    let got = d.search_pattern(&slot("uui?"), wind_dict::WILDCARD_SLOT, 10, true);
    assert_eq!(got.len(), 1);
}
