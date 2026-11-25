//! 真实后端集成测试：默认不跑（`#[ignore]`），由 `tests/run_net_tests.sh` 起容器后以
//! `cargo test --features net --test net_backends -- --ignored` 运行。
//!
//! 环境变量：
//! - WebDAV：`WD_URL` `WD_USER` `WD_PASS`
//! - S3：`S3_ENDPOINT` `S3_BUCKET` `S3_AK` `S3_SK`（区域固定 us-east-1，路径式寻址）
#![cfg(feature = "net")]

use wind_sync::backend::{Backend, BackendError};
use wind_sync::crypto::KdfParams;
use wind_sync::devices::{DeviceInfo, list_devices, put_device_info, remove_device};
use wind_sync::engine::{SyncCtx, sync_once};
use wind_sync::local::MemLocal;
use wind_sync::net::{S3Backend, S3Config, WebDavBackend, WebDavConfig};
use wind_sync::remote;
use wind_sync::state::SyncState;

fn test_ns() -> wind_sync::scope::Namespace {
    wind_sync::scope::Namespace::new(wind_sync::DeviceClass::Desktop, "g")
}

fn env(k: &str) -> String {
    std::env::var(k).unwrap_or_else(|_| panic!("缺少环境变量 {k}（用 run_net_tests.sh 运行）"))
}

fn uniq() -> String {
    let n = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    format!("t{n}")
}

fn webdav(pass: Option<&str>, sub: &str) -> WebDavBackend {
    WebDavBackend::new(&WebDavConfig {
        url: format!("{}/{sub}", env("WD_URL").trim_end_matches('/')),
        username: env("WD_USER"),
        password: pass.map_or_else(|| env("WD_PASS"), str::to_string),
        proxy_from_env: false,
    })
    .unwrap()
}

fn s3(sk: Option<&str>, prefix: &str, page: Option<usize>) -> S3Backend {
    S3Backend::new(&S3Config {
        endpoint: env("S3_ENDPOINT"),
        region: "us-east-1".into(),
        bucket: env("S3_BUCKET"),
        prefix: prefix.into(),
        access_key_id: env("S3_AK"),
        secret_access_key: sk.map_or_else(|| env("S3_SK"), str::to_string),
        path_style: true,
        proxy_from_env: false,
        list_page_size: page,
    })
    .unwrap()
}

fn crud(b: &dyn Backend) {
    let bin: Vec<u8> = (0..100_000u32).map(|i| (i % 251) as u8).collect();
    assert_eq!(b.get("nope/none.bin").unwrap(), None);
    b.delete("nope/none.bin").unwrap(); // 删不存在的键视为成功

    b.put("a/b/c.bin", &bin).unwrap();
    assert_eq!(b.get("a/b/c.bin").unwrap().unwrap(), bin);
    b.put("a/b/c.bin", b"v2").unwrap(); // 覆盖
    assert_eq!(b.get("a/b/c.bin").unwrap().unwrap(), b"v2");

    let weird = "中文 目录/带 空格&符号+(1)/文件 名#1.bin";
    b.put(weird, b"zh").unwrap();
    assert_eq!(b.get(weird).unwrap().unwrap(), b"zh");

    // list：按前缀、子目录递归、部分名前缀
    b.put("a/x.txt", b"x").unwrap();
    b.put("z/y.txt", b"y").unwrap();
    let keys = |p: &str| -> Vec<String> {
        let mut v: Vec<String> = b.list(p).unwrap().into_iter().map(|o| o.key).collect();
        v.sort();
        v
    };
    assert_eq!(keys("a/"), ["a/b/c.bin", "a/x.txt"]);
    assert_eq!(keys("a/b/"), ["a/b/c.bin"]);
    assert_eq!(keys("a/x"), ["a/x.txt"]);
    assert_eq!(keys("中文 目录/"), [weird]);
    assert_eq!(keys("none/"), Vec::<String>::new());
    let m = b.list("a/b/").unwrap();
    assert_eq!(m[0].size, 2);
    assert!(
        m[0].modified.is_some_and(|t| t > 1_700_000_000),
        "{:?}",
        m[0]
    );

    // stat：存在 / 不存在 / 中文路径 / 目录不是对象
    let m = b.stat("a/b/c.bin").unwrap().unwrap();
    assert_eq!((m.key.as_str(), m.size), ("a/b/c.bin", 2));
    assert!(m.modified.is_some_and(|t| t > 1_700_000_000), "{m:?}");
    let m = b.stat(weird).unwrap().unwrap();
    assert_eq!((m.key.as_str(), m.size), (weird, 2));
    assert_eq!(b.stat("a/b/none.bin").unwrap(), None);
    assert_eq!(b.stat("none/none.bin").unwrap(), None);
    assert_eq!(b.stat("a/b").unwrap(), None);

    b.delete("a/b/c.bin").unwrap();
    b.delete(weird).unwrap();
    assert_eq!(b.stat("a/b/c.bin").unwrap(), None);
    assert_eq!(b.get("a/b/c.bin").unwrap(), None);
    assert_eq!(keys("a/"), ["a/x.txt"]);
    b.delete("a/x.txt").unwrap();
    b.delete("z/y.txt").unwrap();
    b.test_connection().unwrap();
}

fn sync_roundtrip(b: &dyn Backend) {
    let dek = remote::init_remote(b, "pw", KdfParams::FAST_TEST).unwrap();
    let mut la = MemLocal::new();
    let mut lb = MemLocal::new();
    let (mut sa, mut sb) = (SyncState::new("devA"), SyncState::new("devB"));
    let mut ctx = SyncCtx::new(1_795_478_400, test_ns());
    ctx.force = true;
    la.set("config.toml", "a = 1\n");
    la.set("themes/中文 主题.json", "{}");
    let r = sync_once(b, &mut la, &mut sa, &dek, &ctx).unwrap();
    assert_eq!(r.uploaded.len(), 2, "{r:?}");
    let r = sync_once(b, &mut lb, &mut sb, &dek, &ctx).unwrap();
    assert_eq!(r.downloaded.len(), 2, "{r:?}");
    assert_eq!(lb.text("config.toml").unwrap(), "a = 1\n");
    assert_eq!(lb.text("themes/中文 主题.json").unwrap(), "{}");
    // 反向：B 改、A 拉
    lb.set("config.toml", "a = 2\n");
    sync_once(b, &mut lb, &mut sb, &dek, &ctx).unwrap();
    sync_once(b, &mut la, &mut sa, &dek, &ctx).unwrap();
    assert_eq!(la.text("config.toml").unwrap(), "a = 2\n");
    // 稳态：无动作
    let r = sync_once(b, &mut la, &mut sa, &dek, &ctx).unwrap();
    assert!(r.uploaded.is_empty() && r.downloaded.is_empty(), "{r:?}");

    // 设备名册：info 与清单同在设备目录下，移除即整目录清掉
    let info = |name: &str| DeviceInfo {
        name: name.into(),
        platform: "linux".into(),
        app_version: "0.0.1".into(),
        caps: vec!["files".into()],
        last_seen: 1_795_478_400,
        group: String::new(),
        group_name: String::new(),
        group_name_at: 0,
    };
    put_device_info(b, &dek.devinfo_key(), "devA", &info("甲")).unwrap();
    put_device_info(b, &dek.devinfo_key(), "devB", &info("乙")).unwrap();
    let l = list_devices(b, &dek.devinfo_key()).unwrap();
    let got: Vec<_> = l
        .iter()
        .map(|(d, i)| (d.as_str(), i.as_ref().unwrap().name.as_str()))
        .collect();
    assert_eq!(got, [("devA", "甲"), ("devB", "乙")]);
    assert!(remove_device(b, "devB").unwrap() >= 2, "各代清单 + info");
    let l = list_devices(b, &dek.devinfo_key()).unwrap();
    assert_eq!(l.len(), 1);
    assert_eq!(l[0].0, "devA");
}

#[test]
#[ignore = "需要 WebDAV 服务，见 run_net_tests.sh"]
fn webdav_crud_and_list() {
    crud(&webdav(None, &uniq()));
}

#[test]
#[ignore = "需要 WebDAV 服务，见 run_net_tests.sh"]
fn webdav_bad_credentials_is_auth() {
    let b = webdav(Some("wrong-password"), &uniq());
    assert!(matches!(b.test_connection(), Err(BackendError::Auth(_))));
    assert!(matches!(b.list(""), Err(BackendError::Auth(_))));
}

#[test]
#[ignore = "需要 WebDAV 服务，见 run_net_tests.sh"]
fn webdav_sync_roundtrip() {
    sync_roundtrip(&webdav(None, &uniq()));
}

#[test]
#[ignore = "需要 MinIO，见 run_net_tests.sh"]
fn s3_crud_and_list() {
    crud(&s3(None, &uniq(), None));
}

#[test]
#[ignore = "需要 MinIO，见 run_net_tests.sh"]
fn s3_list_paginates() {
    let b = s3(None, &uniq(), Some(3));
    for i in 0..10 {
        b.put(&format!("p/k{i:02}.bin"), b"x").unwrap();
    }
    b.put("q/other.bin", b"x").unwrap();
    let mut keys: Vec<String> = b.list("p/").unwrap().into_iter().map(|o| o.key).collect();
    keys.sort();
    assert_eq!(keys.len(), 10, "{keys:?}");
    assert_eq!(keys[0], "p/k00.bin");
    assert_eq!(b.list("").unwrap().len(), 11);
}

#[test]
#[ignore = "需要 MinIO，见 run_net_tests.sh"]
fn s3_bad_credentials_is_auth() {
    let b = s3(Some("wrong-secret"), &uniq(), None);
    assert!(matches!(b.test_connection(), Err(BackendError::Auth(_))));
    assert!(matches!(b.list(""), Err(BackendError::Auth(_))));
}

#[test]
#[ignore = "需要 MinIO，见 run_net_tests.sh"]
fn s3_sync_roundtrip() {
    sync_roundtrip(&s3(None, &uniq(), None));
}

#[test]
#[ignore = "无需服务；验证网络不通归类为 Transient"]
fn unreachable_is_transient() {
    let b = WebDavBackend::new(&WebDavConfig {
        url: "http://127.0.0.1:1/dav".into(),
        username: "u".into(),
        password: "p".into(),
        proxy_from_env: false,
    })
    .unwrap();
    assert!(matches!(
        b.test_connection(),
        Err(BackendError::Transient(_))
    ));
}
