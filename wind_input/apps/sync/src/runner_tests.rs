use super::*;
use crate::secrets::{FileSecretStore, Secrets};
use crate::testing::{FakeCore, MemFactory};
use wind_sync::{KdfParams, MemBackend};

fn runner(dir: &std::path::Path) -> (Runner, Receiver<Input>) {
    let state_dir = dir.join("state");
    let store = FileSecretStore::new(state_dir.join(crate::secrets::FILE_NAME));
    Secrets {
        dek: Some(Dek::generate().to_base64()),
        password: Some("pw".into()),
    }
    .save(&store)
    .unwrap();
    let (tx, rx) = std::sync::mpsc::channel();
    let cfg = Config {
        userdata_dir: dir.join("user"),
        secrets: Box::new(store),
        state_dir,
        settings: SyncSettings {
            enabled: true,
            backend: Some(Target::Webdav {
                url: "http://127.0.0.1/x".into(),
                user: "u".into(),
            }),
            ..Default::default()
        },
        link: Arc::new(FakeCore::default()),
        factory: Box::new(MemFactory(MemBackend::new())),
        timing: Timing::default(),
        kdf: KdfParams::FAST_TEST,
        watch: false,
        class: DeviceClass::Desktop,
        on_demand: false,
    };
    (Runner::new(cfg, tx), rx)
}

/// 回声记录在下一轮扫描之后清掉，本轮重新写过的保留。
#[test]
fn echo_entries_from_earlier_rounds_are_dropped_after_the_next_scan() {
    let d = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(d.path().join("user")).unwrap();
    let (mut r, rx) = runner(d.path());
    lock(&r.echo).insert("config.toml".into(), "h1".into());
    r.start_round();
    let round = loop {
        if let Input::RoundDone(x) = rx.recv_timeout(Duration::from_secs(10)).unwrap() {
            break x;
        }
    };
    // 工作线程里改写了的条目（模拟本轮写入）不得被清
    lock(&r.echo).insert("themes/t/a.toml".into(), "h2".into());
    r.finish_round(*round);
    let e = lock(&r.echo);
    assert!(!e.contains_key("config.toml"), "旧条目应被清: {e:?}");
    assert_eq!(e.get("themes/t/a.toml").map(String::as_str), Some("h2"));
}

fn info_at(t: u64) -> DeviceInfo {
    local_info(&SyncSettings::default(), t)
}

/// 设备信息：没写过必写；同一 UTC 日不重写；跨日才刷新；改名随时写。
#[test]
fn device_info_refresh_rules() {
    let d0 = 20_000 * DAY_SECS;
    let written = info_at(d0 + 100);
    assert!(info_stale(None, &info_at(d0), true));
    assert!(!info_stale(
        Some(&written),
        &info_at(d0 + DAY_SECS - 101),
        true
    ));
    assert!(info_stale(Some(&written), &info_at(d0 + DAY_SECS), true));
    assert!(!info_stale(
        Some(&written),
        &info_at(d0 + 5 * DAY_SECS),
        false
    ));
    let mut renamed = info_at(d0 + 200);
    renamed.name = "别名".into();
    assert!(info_stale(Some(&written), &renamed, false));
    let mut caps = info_at(d0 + 200);
    caps.caps.push("clip".into());
    assert!(info_stale(Some(&written), &caps, false));
}
