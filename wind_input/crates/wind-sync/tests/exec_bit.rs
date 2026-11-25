//! 可执行位随清单条目同步。

use wind_sync::backend::MemBackend;
use wind_sync::crypto::{Dek, KdfParams};
use wind_sync::engine::{SyncCtx, sync_once};
use wind_sync::local::{LocalStore, MemLocal};
use wind_sync::remote;
use wind_sync::scope::{DeviceClass, Namespace};
use wind_sync::state::SyncState;

const T0: u64 = 1_795_478_400;

fn run(b: &MemBackend, dek: &Dek, name: &str, local: &mut MemLocal, st: &mut SyncState) {
    let _ = name;
    let mut ctx = SyncCtx::new(T0, Namespace::new(DeviceClass::Desktop, "pcs"));
    ctx.force = true;
    sync_once(b, local, st, dek, &ctx).expect("同步应成功");
}

#[test]
fn exec_flag_roundtrips() {
    let b = MemBackend::new();
    let dek: Dek = remote::init_remote(&b, "pw", KdfParams::FAST_TEST).unwrap();
    let (mut la, mut sa) = (MemLocal::new(), SyncState::new("a"));
    let (mut lb, mut sb) = (MemLocal::new(), SyncState::new("b"));
    la.set("extensions/k/run", "bin");
    la.set("extensions/k/extension.toml", "x");
    la.exec.insert("extensions/k/run".into());
    run(&b, &dek, "a", &mut la, &mut sa);
    run(&b, &dek, "b", &mut lb, &mut sb);
    assert_eq!(lb.text("extensions/k/run").as_deref(), Some("bin"));
    assert!(lb.is_exec("extensions/k/run").unwrap());
    assert!(!lb.is_exec("extensions/k/extension.toml").unwrap());
    // 普通文件的条目序列化后不含 exec 字段（状态里的键带类别前缀）
    let entry = |suffix: &str| {
        sa.files
            .iter()
            .find(|(k, _)| k.ends_with(suffix))
            .map(|(_, e)| serde_json::to_string(e).unwrap())
            .unwrap()
    };
    assert!(!entry("k/extension.toml").contains("exec"));
    assert!(entry("k/run").contains("\"exec\":true"));
}
