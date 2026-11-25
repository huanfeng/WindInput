//! 类别与组命名空间：桌面与手机共用同一远端，设置按组、主题按类别。

use wind_sync::backend::MemBackend;
use wind_sync::crypto::{Dek, KdfParams};
use wind_sync::engine::{SyncCtx, SyncReport, sync_once};
use wind_sync::local::MemLocal;
use wind_sync::remote;
use wind_sync::scope::{DeviceClass, Namespace, ScopeFlags};
use wind_sync::state::SyncState;

const T0: u64 = 1_795_478_400;

/// 桌面、手机各自一组。
fn ns(class: DeviceClass) -> Namespace {
    Namespace::new(
        class,
        match class {
            DeviceClass::Desktop => "pcs",
            DeviceClass::Mobile => "phones",
        },
    )
}

struct Dev {
    class: DeviceClass,
    local: MemLocal,
    state: SyncState,
    dek: Dek,
}

impl Dev {
    fn new(name: &str, class: DeviceClass, dek: &Dek) -> Dev {
        Dev {
            class,
            local: MemLocal::new(),
            state: SyncState::new(name),
            dek: dek.clone(),
        }
    }
    fn ok(&mut self, b: &MemBackend) -> SyncReport {
        let mut ctx = SyncCtx::new(ScopeFlags::ALL, T0, ns(self.class));
        ctx.force = true;
        sync_once(b, &mut self.local, &mut self.state, &self.dek, &ctx).expect("同步应成功")
    }
}

fn setup() -> (MemBackend, Dek) {
    let b = MemBackend::new();
    let dek = remote::init_remote(&b, "pw", KdfParams::FAST_TEST).unwrap();
    (b, dek)
}

#[test]
fn config_does_not_cross_classes_but_syncs_within() {
    use DeviceClass::*;
    let (b, dek) = setup();
    let mut pc1 = Dev::new("pc1", Desktop, &dek);
    let mut pc2 = Dev::new("pc2", Desktop, &dek);
    let mut ph = Dev::new("ph1", Mobile, &dek);
    pc1.local.set("config.toml", "who = \"pc\"\n");
    ph.local.set("config.toml", "who = \"phone\"\n");
    pc1.ok(&b);
    ph.ok(&b);
    pc2.ok(&b);
    pc1.ok(&b);
    assert_eq!(pc1.local.text("config.toml").unwrap(), "who = \"pc\"\n");
    assert_eq!(ph.local.text("config.toml").unwrap(), "who = \"phone\"\n");
    assert_eq!(pc2.local.text("config.toml").unwrap(), "who = \"pc\"\n");
    // 同类别：第二台手机拿到手机的配置
    let mut ph2 = Dev::new("ph2", Mobile, &dek);
    ph2.ok(&b);
    assert_eq!(ph2.local.text("config.toml").unwrap(), "who = \"phone\"\n");
}

#[test]
fn dicts_and_schemas_cross_classes() {
    use DeviceClass::*;
    let (b, dek) = setup();
    let mut pc = Dev::new("pc1", Desktop, &dek);
    let mut ph = Dev::new("ph1", Mobile, &dek);
    pc.local.set("data/phrases.dict.yaml", "x");
    pc.local.set("schemas/a.schema.yaml", "s");
    pc.local.set("themes/t/theme.json", "{}");
    pc.ok(&b);
    let r = ph.ok(&b);
    assert_eq!(
        ph.local.text("data/phrases.dict.yaml").as_deref(),
        Some("x")
    );
    assert_eq!(ph.local.text("schemas/a.schema.yaml").as_deref(), Some("s"));
    assert!(ph.local.text("themes/t/theme.json").is_none());
    assert_eq!(r.downloaded.len(), 2);
}

#[test]
fn remote_paths_carry_group_and_class_prefix() {
    use DeviceClass::*;
    let (b, dek) = setup();
    let mut ph = Dev::new("ph1", Mobile, &dek);
    ph.local.set("config.toml", "a = 1\n");
    ph.local.set("themes/t/theme.json", "{}");
    ph.local.set("data/phrases.dict.yaml", "p");
    let r = ph.ok(&b);
    // 报告用本机路径
    assert!(r.uploaded.contains(&"config.toml".to_string()));
    assert_eq!(ph.state.files.len(), 3);
    for k in [
        "groups/phones/config.toml",
        "mobile/themes/t/theme.json",
        "data/phrases.dict.yaml",
    ] {
        assert!(
            ph.state.files.contains_key(k),
            "{k}: {:?}",
            ph.state.files.keys()
        );
    }
}

#[test]
fn conflict_copy_lands_on_the_local_path() {
    use DeviceClass::*;
    let (b, dek) = setup();
    let mut a = Dev::new("ph1", Mobile, &dek);
    let mut c = Dev::new("ph2", Mobile, &dek);
    a.local.set("themes/t/a.json", "0");
    a.ok(&b);
    c.ok(&b);
    a.local.set("themes/t/a.json", "A");
    c.local.set("themes/t/a.json", "C");
    a.ok(&b);
    let r = c.ok(&b);
    let copy = r.conflicts[0]
        .copy
        .clone()
        .expect("不可合并文件应有冲突副本");
    assert!(copy.starts_with("themes/t/a.conflict-"), "{copy}");
    assert!(c.local.text(&copy).is_some());
    assert!(c.local.files.keys().all(|k| !k.starts_with("mobile/")));
}

#[test]
fn mass_delete_guard_counts_mapped_paths() {
    use DeviceClass::*;
    let (b, dek) = setup();
    let mut ph = Dev::new("ph1", Mobile, &dek);
    for i in 0..30 {
        ph.local.set(&format!("themes/t/f{i}.json"), "x");
    }
    ph.ok(&b);
    ph.local.files.clear();
    let mut ctx = SyncCtx::new(ScopeFlags::ALL, T0, ns(Mobile));
    ctx.force = true;
    let e = sync_once(&b, &mut ph.local, &mut ph.state, &ph.dek, &ctx).unwrap_err();
    assert!(e.is_mass_deletion(), "{e}");
}
