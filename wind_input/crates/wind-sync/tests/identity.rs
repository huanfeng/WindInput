//! 设备组身份操作：加入（两个方向）、退出、认领、改号。

use wind_sync::backend::{Backend, MemBackend};
use wind_sync::crypto::{Dek, KdfParams};
use wind_sync::devices::{DeviceInfo, put_device_info, remove_device};
use wind_sync::engine::{SyncCtx, sync_once};
use wind_sync::identity::{Overwrite, claim_device, join_group, leave_group, rename_device};
use wind_sync::local::MemLocal;
use wind_sync::manifest::{self, Entry, Manifest};
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
        let mut state = SyncState::new(name);
        state.group = group.into();
        Dev {
            ns: Namespace::new(DeviceClass::Desktop, group),
            local: MemLocal::new(),
            state,
            dek: dek.clone(),
        }
    }
    fn sync(&mut self, b: &MemBackend) {
        let mut ctx = SyncCtx::new(ScopeFlags::ALL, T0, self.ns.clone());
        ctx.force = true;
        ctx.allow_mass_delete = true;
        sync_once(b, &mut self.local, &mut self.state, &self.dek, &ctx).expect("同步应成功");
    }
    fn join(
        &mut self,
        b: &MemBackend,
        group: &str,
        mode: Overwrite,
    ) -> wind_sync::identity::JoinReport {
        self.ns = Namespace::new(DeviceClass::Desktop, group);
        join_group(
            b,
            &mut self.local,
            &mut self.state,
            &self.dek,
            &self.ns,
            mode,
        )
        .expect("加入应成功")
    }
}

fn round(devs: &mut [&mut Dev], b: &MemBackend) {
    for _ in 0..2 {
        for d in devs.iter_mut() {
            d.sync(b);
        }
    }
}

/// a、b 在 home，c 在 office，各自先同步过。
fn three() -> (MemBackend, Dek, Dev, Dev, Dev) {
    let b = MemBackend::new();
    let dek = remote::init_remote(&b, "pw", KdfParams::FAST_TEST).unwrap();
    let mut a = Dev::new("a", "home", &dek);
    let mut bb = Dev::new("b", "home", &dek);
    let mut c = Dev::new("c", "office", &dek);
    a.local.set("config.toml", "home=1");
    a.local.set("data/pinyin/dicts.toml", "d=1");
    a.local.set("data/pinyin/user.dict.yaml", "词\t1");
    c.local.set("config.toml", "office=1");
    c.local.set("data/x/dicts.toml", "x=1");
    c.local.set("compat.toml", "c=1");
    round(&mut [&mut a, &mut bb, &mut c], &b);
    (b, dek, a, bb, c)
}

#[test]
fn join_theirs_replaces_settings_tree() {
    let (b, _dek, mut a, mut bb, mut c) = three();
    let rep = c.join(&b, "home", Overwrite::Theirs);
    assert_eq!(c.local.text("config.toml").as_deref(), Some("home=1"));
    assert_eq!(
        c.local.text("data/pinyin/dicts.toml").as_deref(),
        Some("d=1")
    );
    assert!(c.local.text("data/x/dicts.toml").is_none());
    assert!(c.local.text("compat.toml").is_none());
    assert!(rep.deleted.contains(&"data/x/dicts.toml".to_string()));
    assert!(rep.written.contains(&"config.toml".to_string()));
    assert_eq!(c.state.group, "home");
    round(&mut [&mut c, &mut a, &mut bb], &b);
    assert_eq!(a.local.text("config.toml").as_deref(), Some("home=1"));
    assert!(a.local.text("data/x/dicts.toml").is_none());
    assert!(bb.local.text("data/x/dicts.toml").is_none());
    assert_eq!(c.local.text("config.toml").as_deref(), Some("home=1"));
}

#[test]
fn join_mine_overwrites_group() {
    let (b, _dek, mut a, mut bb, mut c) = three();
    let rep = c.join(&b, "home", Overwrite::Mine);
    assert!(rep.written.is_empty() && rep.deleted.is_empty());
    assert_eq!(c.local.text("config.toml").as_deref(), Some("office=1"));
    round(&mut [&mut c, &mut a, &mut bb], &b);
    assert_eq!(a.local.text("config.toml").as_deref(), Some("office=1"));
    assert_eq!(bb.local.text("config.toml").as_deref(), Some("office=1"));
    assert_eq!(a.local.text("data/x/dicts.toml").as_deref(), Some("x=1"));
    // home 有而 c 没有的设置文件被删除
    assert!(a.local.text("data/pinyin/dicts.toml").is_none());
    assert!(bb.local.text("data/pinyin/dicts.toml").is_none());
}

#[test]
fn leave_group_keeps_local_and_isolates() {
    let (b, _dek, mut a, mut bb, _c) = three();
    leave_group(&mut a.state, "a");
    a.ns = Namespace::new(DeviceClass::Desktop, "a");
    round(&mut [&mut a, &mut bb], &b);
    assert_eq!(a.local.text("config.toml").as_deref(), Some("home=1"));
    bb.local.set("config.toml", "home=2");
    round(&mut [&mut bb, &mut a], &b);
    assert_eq!(a.local.text("config.toml").as_deref(), Some("home=1"));
    // 数据仍共享
    assert!(a.local.text("data/pinyin/user.dict.yaml").is_some());
}

#[test]
fn claim_restores_settings() {
    let (b, dek, a, _bb, _c) = three();
    let mut n = Dev::new("tmp-new", "", &dek);
    let info = DeviceInfo {
        name: "a".into(),
        platform: "linux".into(),
        app_version: "0".into(),
        caps: vec![],
        last_seen: T0,
        group: "home".into(),
        group_name: "家".into(),
        group_name_at: 5,
    };
    put_device_info(&b, &dek.devinfo_key(), "a", &info).unwrap();
    let g = claim_device(&b, &mut n.state, &dek, "a").unwrap();
    assert_eq!(g, "home");
    assert_eq!(n.state.device, "a");
    assert_eq!(n.state.group_name, "家");
    assert!(n.state.identity_confirmed);
    n.join(&b, &g, Overwrite::Theirs);
    n.sync(&b);
    assert_eq!(n.local.text("config.toml"), a.local.text("config.toml"));
    assert_eq!(
        n.local.text("data/pinyin/dicts.toml").as_deref(),
        Some("d=1")
    );
}

#[test]
fn claim_without_info_uses_device_as_group() {
    let (b, dek, _a, _bb, _c) = three();
    let mut st = SyncState::new("x");
    assert_eq!(claim_device(&b, &mut st, &dek, "c").unwrap(), "c");
}

#[test]
fn rename_keeps_files_reachable() {
    let (b, dek, mut a, mut bb, mut c) = three();
    rename_device(&b, &mut a.state, "home-pc").unwrap();
    assert_eq!(a.state.retired_ids, vec!["a".to_string()]);
    a.sync(&b);
    remove_device(&b, "a").unwrap();
    round(&mut [&mut bb, &mut c], &b);
    let mut d = Dev::new("d", "home", &dek);
    d.sync(&b);
    assert_eq!(d.local.text("config.toml").as_deref(), Some("home=1"));
    assert_eq!(
        d.local.text("data/pinyin/dicts.toml").as_deref(),
        Some("d=1")
    );
    assert_eq!(
        d.local.text("data/pinyin/user.dict.yaml").as_deref(),
        Some("词\t1")
    );
}

#[test]
fn rename_rejects_taken_id() {
    let (b, _dek, mut a, _bb, _c) = three();
    let before = a.state.clone();
    assert!(rename_device(&b, &mut a.state, "b").is_err());
    assert!(rename_device(&b, &mut a.state, "Bad Id").is_err());
    assert_eq!(a.state, before);
}

#[test]
fn join_theirs_ignores_out_of_scope_remote_entries() {
    let (b, dek, _a, _bb, mut c) = three();
    let ent = |h: &str| Entry {
        hash: h.into(),
        base: None,
        deleted: false,
        lamport: 9999,
        device: "evil".into(),
        absorbed: vec![],
    };
    let mut files = std::collections::BTreeMap::new();
    files.insert("groups/home/data/../dicts.toml".to_string(), ent("aa"));
    files.insert("groups/home/config.toml/../../x".to_string(), ent("bb"));
    let m = Manifest {
        device: "evil".into(),
        gen_: 1,
        lamport: 9999,
        files,
        nonce: "n".into(),
        prev: String::new(),
    };
    b.put(
        &manifest::manifest_key("evil", 1),
        &m.encode(&dek.files_key()),
    )
    .unwrap();
    let before = c.local.files.clone();
    c.join(&b, "home", Overwrite::Theirs);
    assert!(c.local.files.keys().all(|k| !k.contains("..") && k != "x"));
    assert!(c.state.files.keys().all(|k| !k.contains("..")));
    assert_eq!(c.local.text("config.toml").as_deref(), Some("home=1"));
    assert_ne!(before, c.local.files);
}

#[test]
fn join_theirs_is_atomic_on_missing_blob() {
    let (b, _dek, _a, _bb, mut c) = three();
    for k in b.keys(&manifest::blobs_prefix()) {
        b.delete(&k).unwrap();
    }
    let (files, state) = (c.local.files.clone(), c.state.clone());
    c.ns = Namespace::new(DeviceClass::Desktop, "home");
    let r = join_group(
        &b,
        &mut c.local,
        &mut c.state,
        &c.dek,
        &c.ns,
        Overwrite::Theirs,
    );
    assert!(r.is_err());
    assert_eq!(c.local.files, files);
    assert_eq!(c.state, state);
}

#[test]
fn join_lifts_lamport_above_group_entries() {
    let (b, _dek, _a, _bb, mut c) = three();
    c.join(&b, "home", Overwrite::Theirs);
    let max = c.state.files.values().map(|e| e.lamport).max().unwrap();
    assert!(c.state.lamport >= max);
}
