//! 设备组命名空间：数据全员共享，设置按组隔离。

use wind_sync::backend::MemBackend;
use wind_sync::crypto::Dek;
use wind_sync::engine::{SyncCtx, sync_once};
use wind_sync::local::MemLocal;
use wind_sync::remote;
use wind_sync::scope::{DeviceClass, Namespace};
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
        let mut ctx = SyncCtx::new(T0, self.ns.clone());
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
    let dek = remote::init_remote(&b).unwrap();
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

/// 同组两设备对设置文件造不可合并的冲突：同步成功、本机出现副本、下一轮仍成功。
#[test]
fn setting_conflict_copies_are_written_locally() {
    for path in ["config.toml", "data/pinyin/dicts.toml"] {
        let b = MemBackend::new();
        let dek = remote::init_remote(&b).unwrap();
        let mut a = Dev::new("a", "home", &dek);
        let mut bb = Dev::new("b", "home", &dek);
        a.local.set(path, "x = 1\n");
        round(&mut [&mut a, &mut bb], &b);
        // 远端是坏 TOML，本机另有修改：无法合并
        a.local.set(path, "x = [\n");
        a.sync(&b);
        bb.local.set(path, "x = 2\n");
        bb.sync(&b);
        let copies: Vec<String> = bb
            .local
            .files
            .keys()
            .filter(|p| p.contains(".conflict-a-"))
            .cloned()
            .collect();
        assert_eq!(copies.len(), 1, "{path}: {:?}", bb.local.files.keys());
        assert_eq!(bb.local.text(&copies[0]).as_deref(), Some("x = [\n"));
        assert!(!copies[0].starts_with("groups/"), "{}", copies[0]);
        bb.sync(&b);
        a.sync(&b);
    }
}
