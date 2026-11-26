use std::collections::BTreeMap;
use wind_sync::backend::{Backend, MemBackend};
use wind_sync::crypto::{Dek, FilesKey};
use wind_sync::learn::lane::{GC_MIN_AGE_SECS, Index, fetch_peers, gc_own, publish, purge_device};
use wind_sync::manifest::{learn_index_key, learn_prefix};

fn fk() -> FilesKey {
    Dek::generate().files_key()
}

fn objs(pairs: &[(&str, &str)]) -> BTreeMap<String, Vec<u8>> {
    pairs
        .iter()
        .map(|(k, v)| (k.to_string(), v.as_bytes().to_vec()))
        .collect()
}

fn known(dev: &str, idx: &Index) -> BTreeMap<String, Index> {
    BTreeMap::from([(dev.to_string(), idx.clone())])
}

#[test]
fn publish_uploads_only_changed() {
    let (b, k) = (MemBackend::new(), fk());
    let i1 = publish(&b, &k, "b", None, &objs(&[("a", "1"), ("c", "2")])).unwrap();
    assert_eq!(i1.gen_, 1);
    b.reset_calls();
    let i2 = publish(&b, &k, "b", Some(&i1), &objs(&[("a", "1"), ("c", "3")])).unwrap();
    assert_eq!(i2.gen_, 2);
    assert_eq!(b.calls().put, 2, "1 个 obj + 1 个 index");
}

#[test]
fn old_indexes_beyond_two_generations_deleted() {
    let (b, k) = (MemBackend::new(), fk());
    let mut prev = None;
    for i in 0..4 {
        let v = i.to_string();
        prev = Some(publish(&b, &k, "b", prev.as_ref(), &objs(&[("a", &v)])).unwrap());
    }
    let idx: Vec<_> = b
        .keys(&learn_prefix("b"))
        .into_iter()
        .filter(|k| k.contains("index."))
        .collect();
    assert_eq!(idx, vec![learn_index_key("b", 3), learn_index_key("b", 4)]);
}

#[test]
fn peer_sees_latest_index_once() {
    let (b, k) = (MemBackend::new(), fk());
    let i1 = publish(&b, &k, "b", None, &objs(&[("a", "1")])).unwrap();
    let i2 = publish(&b, &k, "b", Some(&i1), &objs(&[("a", "2"), ("n", "x")])).unwrap();
    let ups = fetch_peers(&b, &k, "a", &BTreeMap::new()).unwrap();
    assert_eq!(ups.len(), 1);
    assert_eq!((ups[0].gen_, ups[0].index_ok), (2, true));
    assert_eq!(ups[0].index, i2);
    assert_eq!(ups[0].objects["a"], Some(b"2".to_vec()));
    assert!(
        fetch_peers(&b, &k, "a", &known("b", &i2))
            .unwrap()
            .is_empty()
    );
    // 自己的通道不取
    assert!(
        fetch_peers(&b, &k, "b", &BTreeMap::new())
            .unwrap()
            .is_empty()
    );
}

#[test]
fn only_changed_downloaded_and_deleted_reported() {
    let (b, k) = (MemBackend::new(), fk());
    let i1 = publish(&b, &k, "b", None, &objs(&[("a", "1"), ("c", "2")])).unwrap();
    let i2 = publish(&b, &k, "b", Some(&i1), &objs(&[("a", "1"), ("n", "3")])).unwrap();
    b.reset_calls();
    let ups = fetch_peers(&b, &k, "a", &known("b", &i1)).unwrap();
    assert_eq!(ups[0].index, i2);
    assert_eq!(ups[0].objects.len(), 2);
    assert_eq!(ups[0].objects["c"], None);
    assert_eq!(ups[0].objects["n"], Some(b"3".to_vec()));
    // 1 次目录 + 1 个新对象
    assert_eq!(b.calls().get, 2);
}

#[test]
fn stale_index_ignored() {
    let (b, k) = (MemBackend::new(), fk());
    let i1 = publish(&b, &k, "b", None, &objs(&[("a", "1")])).unwrap();
    let seen2 = Index {
        gen_: 2,
        objects: i1.objects.clone(),
    };
    assert!(
        fetch_peers(&b, &k, "a", &known("b", &seen2))
            .unwrap()
            .is_empty()
    );
    let i2 = publish(&b, &k, "b", Some(&i1), &objs(&[("a", "2")])).unwrap();
    let i3 = publish(&b, &k, "b", Some(&i2), &objs(&[("a", "3")])).unwrap();
    let ups = fetch_peers(&b, &k, "a", &known("b", &seen2)).unwrap();
    assert_eq!(ups[0].gen_, i3.gen_);
}

#[test]
fn corrupt_peer_object_is_skipped() {
    let (b, k) = (MemBackend::new(), fk());
    let i = publish(&b, &k, "b", None, &objs(&[("a", "1"), ("c", "2")])).unwrap();
    let key = b.keys(&format!("{}obj/", learn_prefix("b"))).remove(0);
    let mut raw = b.raw_get(&key).unwrap();
    *raw.last_mut().unwrap() ^= 1;
    b.raw_put(&key, raw);
    let ups = fetch_peers(&b, &k, "a", &BTreeMap::new()).unwrap();
    assert!(ups[0].index_ok);
    assert_eq!(ups[0].objects.len(), 1);
    assert_eq!(ups[0].warnings.len(), 1);
    assert_eq!(ups[0].index, i);
}

#[test]
fn bad_index_skips_device_without_advancing() {
    let (b, k) = (MemBackend::new(), fk());
    let i1 = publish(&b, &k, "b", None, &objs(&[("a", "1")])).unwrap();
    b.raw_put(&learn_index_key("b", 2), b"garbage".to_vec());
    let ups = fetch_peers(&b, &k, "a", &known("b", &i1)).unwrap();
    assert!(!ups[0].index_ok);
    assert_eq!(ups[0].gen_, 1);
    assert!(ups[0].objects.is_empty());
    assert_eq!(ups[0].warnings.len(), 1);
}

#[test]
fn index_moved_from_other_device_or_number_rejected() {
    let (b, k) = (MemBackend::new(), fk());
    publish(&b, &k, "b", None, &objs(&[("a", "1")])).unwrap();
    publish(&b, &k, "c", None, &objs(&[("a", "1")])).unwrap();
    // 把 b 的 index.1 放到 c 的 index.5 下
    let raw = b.raw_get(&learn_index_key("b", 1)).unwrap();
    b.raw_put(&learn_index_key("c", 5), raw);
    let ups = fetch_peers(&b, &k, "a", &BTreeMap::new()).unwrap();
    let c = ups.iter().find(|u| u.dev == "c").unwrap();
    assert!(!c.index_ok);
}

#[test]
fn invalid_object_name_rejected_on_publish() {
    let (b, k) = (MemBackend::new(), fk());
    assert!(publish(&b, &k, "b", None, &objs(&[("../x", "1")])).is_err());
}

#[test]
fn gc_own_keeps_referenced_and_recent() {
    let (b, k) = (MemBackend::new(), fk());
    b.set_now(1000);
    let i1 = publish(&b, &k, "b", None, &objs(&[("a", "1")])).unwrap();
    let i2 = publish(&b, &k, "b", Some(&i1), &objs(&[("a", "2")])).unwrap();
    let now = 1000 + GC_MIN_AGE_SECS;
    // 两代都被引用：不删
    assert_eq!(gc_own(&b, &k, "b", &[&i1, &i2], now).unwrap(), 0);
    // 旧代不再引用但太新：不删
    assert_eq!(gc_own(&b, &k, "b", &[&i2], now - 1).unwrap(), 0);
    // 旧且无引用：删 1 个
    assert_eq!(gc_own(&b, &k, "b", &[&i2], now).unwrap(), 1);
    assert_eq!(gc_own(&b, &k, "b", &[&i2], now).unwrap(), 0);
    assert!(
        fetch_peers(&b, &k, "a", &BTreeMap::new()).unwrap()[0]
            .warnings
            .is_empty()
    );
}

#[test]
fn purge_removes_only_that_device() {
    let (b, k) = (MemBackend::new(), fk());
    publish(&b, &k, "ab", None, &objs(&[("a", "1")])).unwrap();
    publish(&b, &k, "abc", None, &objs(&[("a", "1")])).unwrap();
    let n = purge_device(&b, "ab").unwrap();
    assert_eq!(n, 2);
    assert!(b.keys(&learn_prefix("ab")).is_empty());
    assert_eq!(b.keys(&learn_prefix("abc")).len(), 2);
    assert!(purge_device(&b, "").is_err());
    let _ = b.list("");
}

#[test]
fn invalid_object_name_in_remote_index_skipped_with_warning() {
    use wind_sync::crypto::{ObjKind, seal};
    let (b, k) = (MemBackend::new(), fk());
    let json = br#"{"gen":1,"objects":{"../evil":"00","ok":"11"}}"#;
    b.raw_put(
        &learn_index_key("b", 1),
        seal(&k, ObjKind::Learn, "b/index.1", json),
    );
    let ups = fetch_peers(&b, &k, "a", &BTreeMap::new()).unwrap();
    assert!(ups[0].index_ok);
    assert!(!ups[0].objects.contains_key("../evil"));
    // ../evil 的名字警告 + ok 对象缺失警告
    assert_eq!(ups[0].warnings.len(), 2);
}
