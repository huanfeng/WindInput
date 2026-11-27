//! 远端空间占用：按前缀求和与分类、等待回收、学习数据与统计按对象名拆分。

use std::collections::BTreeMap;
use wind_sync::backend::{Backend, BackendError, MemBackend, ProviderUsage};
use wind_sync::crypto::{Dek, blob_id, content_hash};
use wind_sync::engine::{SyncCtx, sync_once};
use wind_sync::learn::lane::publish;
use wind_sync::local::MemLocal;
use wind_sync::manifest::{self, ROOT};
use wind_sync::remote;
use wind_sync::state::SyncState;
use wind_sync::usage::compute;

const T0: u64 = 1_795_478_400;

fn ns() -> wind_sync::scope::Namespace {
    wind_sync::scope::Namespace::new(wind_sync::DeviceClass::Desktop, "g")
}

fn sync(b: &MemBackend, local: &mut MemLocal, state: &mut SyncState, dek: &Dek) {
    let mut ctx = SyncCtx::new(T0, ns());
    ctx.force = true;
    sync_once(b, local, state, dek, &ctx).expect("同步应成功");
}

fn size_of(b: &MemBackend, prefix: &str) -> u64 {
    b.list(prefix).unwrap().iter().map(|o| o.size).sum()
}

fn objs(pairs: &[(&str, &str)]) -> BTreeMap<String, Vec<u8>> {
    pairs
        .iter()
        .map(|(k, v)| (k.to_string(), v.as_bytes().to_vec()))
        .collect()
}

#[test]
fn sums_by_prefix_and_splits_learned_from_stats() {
    let b = MemBackend::new();
    let dek = remote::init_remote(&b).unwrap();
    let (mut local, mut state) = (MemLocal::new(), SyncState::new("pc"));
    local.set("config.toml", "a = 1\n");
    local.set("themes/t.json", "{}");
    sync(&b, &mut local, &mut state, &dek);
    let fk = dek.files_key();
    let stats_body = "x".repeat(300);
    publish(
        &b,
        &fk,
        "pc",
        "",
        None,
        &objs(&[
            ("pinyin/freq.base", "ni\t3\n"),
            ("stats/2026-11.toml", &stats_body),
        ]),
    )
    .unwrap();
    publish(
        &b,
        &fk,
        "gone",
        "",
        None,
        &objs(&[("pinyin/freq.base", "a")]),
    )
    .unwrap();
    b.raw_put(&format!("{ROOT}/blobs/orphan"), vec![0; 100]);
    b.raw_put("elsewhere/big", vec![0; 5000]);

    let u = compute(&b, &dek).unwrap();
    let root = format!("{ROOT}/");
    assert_eq!(u.total, size_of(&b, &root), "只算同步根之内");
    assert_eq!(u.blobs, size_of(&b, &manifest::blobs_prefix()));
    assert_eq!(u.blobs_unreferenced, Some(100), "孤儿 blob 等待回收");

    let stats_key =
        manifest::learn_obj_key("pc", &blob_id(&fk, &content_hash(stats_body.as_bytes())));
    let stats_size = b.stat(&stats_key).unwrap().unwrap().size;
    let ids: Vec<&str> = u.devices.iter().map(|d| d.id.as_str()).collect();
    assert_eq!(ids, ["gone", "pc"], "按设备号排序，名册外的也列出");
    let pc = &u.devices[1];
    assert_eq!(pc.stats, stats_size);
    assert_eq!(
        pc.learned + pc.stats,
        size_of(&b, &manifest::learn_prefix("pc"))
    );
    assert_eq!(u.devices[0].stats, 0);
    let learn_total: u64 = u.devices.iter().map(|d| d.learned + d.stats).sum();
    assert_eq!(u.other, u.total - u.blobs - learn_total);
    assert_eq!(
        u.other,
        size_of(&b, &manifest::devices_prefix()) + size_of(&b, &manifest::meta_key())
    );
}

/// 改过三次的文件：最新两代清单都不再引用第一版，它算等待回收。
#[test]
fn old_versions_beyond_kept_manifests_are_unreferenced() {
    let b = MemBackend::new();
    let dek = remote::init_remote(&b).unwrap();
    let (mut local, mut state) = (MemLocal::new(), SyncState::new("pc"));
    for v in ["a = 1\n", "a = 22\n", "a = 333\n"] {
        local.set("config.toml", v);
        sync(&b, &mut local, &mut state, &dek);
    }
    let first = manifest::blob_key(&blob_id(&dek.files_key(), &content_hash(b"a = 1\n")));
    let first_size = b.stat(&first).unwrap().unwrap().size;
    let u = compute(&b, &dek).unwrap();
    assert_eq!(u.blobs_unreferenced, Some(first_size));
    assert!(u.blobs > first_size);
}

/// 解不开的学习目录（别的密钥写的）：整份算学习数据，不报错。
#[test]
fn unreadable_learn_index_counts_as_learned() {
    let b = MemBackend::new();
    let dek = remote::init_remote(&b).unwrap();
    let foreign = Dek::generate().files_key();
    publish(
        &b,
        &foreign,
        "old",
        "",
        None,
        &objs(&[("stats/2026-10.toml", "s")]),
    )
    .unwrap();
    let u = compute(&b, &dek).unwrap();
    assert_eq!(u.devices.len(), 1);
    assert_eq!(u.devices[0].stats, 0);
    assert_eq!(
        u.devices[0].learned,
        size_of(&b, &manifest::learn_prefix("old"))
    );
}

#[test]
fn backend_error_propagates() {
    let b = MemBackend::new();
    let dek = remote::init_remote(&b).unwrap();
    b.fail_all(Some(BackendError::Transient("断网".into())));
    assert!(compute(&b, &dek).is_err());
}

#[test]
fn provider_usage_defaults_to_none() {
    let b = MemBackend::new();
    assert_eq!(b.usage().unwrap(), None);
    b.set_provider_usage(Some(ProviderUsage {
        used: 10,
        available: Some(90),
        quota: Some(100),
    }));
    assert_eq!(
        b.usage().unwrap(),
        Some(ProviderUsage {
            used: 10,
            available: Some(90),
            quota: Some(100)
        })
    );
}

/// 清单解不开（别的密钥写的残留）：等待回收记为无法确定，其余照常统计。
#[test]
fn unreadable_manifest_leaves_unreferenced_unknown() {
    let b = MemBackend::new();
    let dek = remote::init_remote(&b).unwrap();
    let (mut local, mut state) = (MemLocal::new(), SyncState::new("pc"));
    local.set("config.toml", "a = 1\n");
    sync(&b, &mut local, &mut state, &dek);
    b.raw_put(&manifest::manifest_key("old", 3), vec![7; 64]);
    let u = compute(&b, &dek).unwrap();
    assert_eq!(u.blobs_unreferenced, None);
    assert_eq!(u.total, size_of(&b, &format!("{ROOT}/")));
    assert!(u.blobs > 0);
}

/// 只认最新两代目录：更早一代里的统计对象（若还残留）按学习数据算。
#[test]
fn stats_split_uses_latest_two_indexes_only() {
    let b = MemBackend::new();
    let dek = remote::init_remote(&b).unwrap();
    let fk = dek.files_key();
    let gen1 = objs(&[("stats/2026-09.toml", "old-stats")]);
    let i1 = publish(&b, &fk, "pc", "", None, &gen1).unwrap();
    let i2 = publish(&b, &fk, "pc", "", Some(&i1), &objs(&[("a", "1")])).unwrap();
    // 伪造残留的第 1 代目录（发布第 3 代时本应删掉）
    let idx1 = b.raw_get(&manifest::learn_index_key("pc", 1));
    publish(&b, &fk, "pc", "", Some(&i2), &objs(&[("a", "2")])).unwrap();
    if let Some(raw) = idx1 {
        b.raw_put(&manifest::learn_index_key("pc", 1), raw);
    }
    let old_obj = manifest::learn_obj_key("pc", &blob_id(&fk, &content_hash(b"old-stats")));
    assert!(b.stat(&old_obj).unwrap().is_some(), "旧对象尚未回收");
    let u = compute(&b, &dek).unwrap();
    assert_eq!(u.devices[0].stats, 0, "第 1 代不在最新两代里");
}

#[test]
fn cancel_stops_the_scan() {
    let b = MemBackend::new();
    let dek = remote::init_remote(&b).unwrap();
    let e = wind_sync::usage::compute_with(&b, &dek, &|| true).unwrap_err();
    assert!(e.to_string().contains(wind_sync::usage::CANCELLED));
}
