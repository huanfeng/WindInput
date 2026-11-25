//! 设备组命名空间：数据全员共享，设置按组隔离。

use wind_sync::backend::MemBackend;
use wind_sync::crypto::{Dek, KdfParams};
use wind_sync::engine::{SyncCtx, sync_once};
use wind_sync::local::MemLocal;
use wind_sync::remote;
use wind_sync::scope::{DeviceClass, Namespace, ScopeFlags};
use wind_sync::state::SyncState;

const T0: u64 = 1_795_478_400;

struct Dev {
    ns: Namespace,
    local: MemLocal,
    state: SyncState,
    dek: Dek,
}

impl Dev {
    fn new(name: &str, group: &str, dek: &Dek) -> Dev {
        Dev {
            ns: Namespace::new(DeviceClass::Desktop, group),
            local: MemLocal::new(),
            state: SyncState::new(name),
            dek: dek.clone(),
        }
    }
    fn sync(&mut self, b: &MemBackend) {
        let mut ctx = SyncCtx::new(ScopeFlags::ALL, T0, self.ns.clone());
        ctx.force = true;
        sync_once(b, &mut self.local, &mut self.state, &self.dek, &ctx).expect("同步应成功");
    }
}

fn round(devs: &mut [&mut Dev], b: &MemBackend) {
    for _ in 0..2 {
        for d in devs.iter_mut() {
            d.sync(b);
        }
    }
}

#[test]
fn two_groups_share_data_isolate_settings() {
    let b = MemBackend::new();
    let dek = remote::init_remote(&b, "pw", KdfParams::FAST_TEST).unwrap();
    let mut a = Dev::new("a", "home", &dek);
    let mut bb = Dev::new("b", "home", &dek);
    let mut c = Dev::new("c", "office", &dek);
    a.local.set("config.toml", "x=1");
    a.local.set("data/pinyin/user.dict.yaml", "词\t1");
    round(&mut [&mut a, &mut bb, &mut c], &b);
    assert_eq!(bb.local.text("config.toml").as_deref(), Some("x=1"));
    assert!(c.local.text("config.toml").is_none());
    for d in [&a, &bb, &c] {
        assert!(d.local.text("data/pinyin/user.dict.yaml").is_some());
    }
    c.local.set("config.toml", "x=2");
    round(&mut [&mut a, &mut bb, &mut c], &b);
    assert_eq!(a.local.text("config.toml").as_deref(), Some("x=1"));
    assert_eq!(bb.local.text("config.toml").as_deref(), Some("x=1"));
    assert_eq!(c.local.text("config.toml").as_deref(), Some("x=2"));
}
