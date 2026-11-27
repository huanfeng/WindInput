//! 设备名册：多设备经同一个 MemBackend 互相可见（设计 sync-messaging §3.2）。

use wind_sync::backend::MemBackend;
use wind_sync::crypto::Dek;
use wind_sync::devices::{DeviceInfo, list_devices, put_device_info, remove_device};
use wind_sync::engine::{SyncCtx, sync_once};
use wind_sync::local::MemLocal;
use wind_sync::manifest;
use wind_sync::remote;
use wind_sync::state::SyncState;

fn test_ns() -> wind_sync::scope::Namespace {
    wind_sync::scope::Namespace::new(wind_sync::DeviceClass::Desktop, "g")
}

const T0: u64 = 1_795_478_400;

fn info(name: &str, last_seen: u64) -> DeviceInfo {
    DeviceInfo {
        name: name.into(),
        platform: "linux".into(),
        app_version: "0.0.1".into(),
        caps: vec!["files".into()],
        last_seen,
        group: String::new(),
        group_name: String::new(),
        group_name_at: 0,
    }
}

fn setup() -> (MemBackend, Dek) {
    let b = MemBackend::new();
    let dek = remote::init_remote(&b).unwrap();
    (b, dek)
}

fn names(b: &MemBackend, dek: &Dek) -> Vec<(String, Option<String>)> {
    list_devices(b, &dek.devinfo_key())
        .unwrap()
        .into_iter()
        .map(|(d, r)| (d, r.ok().map(|i| i.name)))
        .collect()
}

#[test]
fn two_devices_see_each_other_and_renames_propagate() {
    let (b, dek) = setup();
    put_device_info(&b, &dek.devinfo_key(), "dev-a", &info("办公室", T0)).unwrap();
    put_device_info(&b, &dek.devinfo_key(), "dev-b", &info("笔记本", T0)).unwrap();
    let seen = names(&b, &dek);
    assert_eq!(
        seen,
        vec![
            ("dev-a".into(), Some("办公室".into())),
            ("dev-b".into(), Some("笔记本".into()))
        ]
    );
    put_device_info(&b, &dek.devinfo_key(), "dev-a", &info("家里", T0 + 5)).unwrap();
    assert_eq!(names(&b, &dek)[0].1.as_deref(), Some("家里"));
}

#[test]
fn removal_drops_device_from_the_other_view() {
    let (b, dek) = setup();
    put_device_info(&b, &dek.devinfo_key(), "dev-a", &info("a", T0)).unwrap();
    put_device_info(&b, &dek.devinfo_key(), "dev-b", &info("b", T0)).unwrap();
    remove_device(&b, "dev-b").unwrap();
    assert_eq!(names(&b, &dek), vec![("dev-a".into(), Some("a".into()))]);
    assert!(b.keys("windinput-sync/v1/devices/dev-b/").is_empty());
}

#[test]
fn removed_device_reappears_when_it_syncs_and_writes_info_again() {
    let (b, dek) = setup();
    let mut local = MemLocal::new();
    let mut st = SyncState::new("dev-b");
    local.set("config.toml", "a = 1\n");
    let mut ctx = SyncCtx::new(T0, test_ns());
    ctx.force = true;
    sync_once(&b, &mut local, &mut st, &dek, &ctx).unwrap();
    put_device_info(&b, &dek.devinfo_key(), "dev-b", &info("b", T0)).unwrap();
    remove_device(&b, "dev-b").unwrap();
    assert!(names(&b, &dek).is_empty());

    // 被移除的设备改了文件再同步：清单重新出现；写回 info 后名册完整。
    local.set("config.toml", "a = 2\n");
    sync_once(&b, &mut local, &mut st, &dek, &ctx).unwrap();
    let after_sync = list_devices(&b, &dek.devinfo_key()).unwrap();
    assert_eq!(after_sync.len(), 1);
    assert!(after_sync[0].1.is_err(), "info 未写回前读不出");
    put_device_info(&b, &dek.devinfo_key(), "dev-b", &info("b", T0 + 1)).unwrap();
    assert_eq!(names(&b, &dek), vec![("dev-b".into(), Some("b".into()))]);
}

#[test]
fn device_with_manifest_but_no_info_is_listed_as_unreadable() {
    let (b, dek) = setup();
    let mut local = MemLocal::new();
    let mut st = SyncState::new("dev-old");
    local.set("config.toml", "a = 1\n");
    let mut ctx = SyncCtx::new(T0, test_ns());
    ctx.force = true;
    sync_once(&b, &mut local, &mut st, &dek, &ctx).unwrap();
    let l = list_devices(&b, &dek.devinfo_key()).unwrap();
    assert_eq!(l.len(), 1);
    assert_eq!(l[0].0, "dev-old");
    assert!(l[0].1.is_err());
}

#[test]
fn sync_and_gc_never_touch_info_objects() {
    let (b, dek) = setup();
    b.set_now(T0);
    put_device_info(&b, &dek.devinfo_key(), "dev-z", &info("z", T0)).unwrap();
    let key = manifest::device_info_key("dev-z");
    let before = b.raw_get(&key).unwrap();
    let mut local = MemLocal::new();
    let mut st = SyncState::new("dev-a");
    local.set("config.toml", "a = 1\n");
    let mut ctx = SyncCtx::new(T0 + 400 * 86400, test_ns());
    ctx.force = true;
    ctx.gc = true;
    sync_once(&b, &mut local, &mut st, &dek, &ctx).unwrap();
    remote::gc_blobs(&b, &dek, T0 + 800 * 86400, 1).unwrap();
    assert_eq!(b.raw_get(&key), Some(before));
    assert!(names(&b, &dek).contains(&("dev-z".into(), Some("z".into()))));
}
