//! 多处共用同一设备号的检测（清单 nonce 链）。

use wind_sync::backend::MemBackend;
use wind_sync::crypto::{Dek, KdfParams};
use wind_sync::engine::{SyncCtx, SyncReport, sync_once};
use wind_sync::error::SyncError;
use wind_sync::local::MemLocal;
use wind_sync::remote;
use wind_sync::scope::{Namespace, ScopeFlags};
use wind_sync::state::SyncState;

const T0: u64 = 1_795_478_400;

struct Dev {
    local: MemLocal,
    state: SyncState,
    dek: Dek,
}

impl Dev {
    fn new(name: &str, dek: &Dek) -> Dev {
        Dev {
            local: MemLocal::new(),
            state: SyncState::new(name),
            dek: dek.clone(),
        }
    }
    fn sync(&mut self, b: &MemBackend) -> Result<SyncReport, SyncError> {
        let mut ctx = SyncCtx::new(
            ScopeFlags::ALL,
            T0,
            Namespace::new(wind_sync::DeviceClass::Desktop, "g"),
        );
        ctx.force = true;
        sync_once(b, &mut self.local, &mut self.state, &self.dek, &ctx)
    }
}

fn setup() -> (MemBackend, Dek) {
    let b = MemBackend::new();
    let dek = remote::init_remote(&b, "pw", KdfParams::FAST_TEST).unwrap();
    (b, dek)
}

#[test]
fn crash_after_publish_is_not_conflict() {
    let (b, dek) = setup();
    let mut a = Dev::new("a", &dek);
    a.local.set("themes/t1.json", "1");
    let before = a.state.clone();
    a.sync(&b).unwrap();
    a.state = before;
    a.sync(&b).expect("清单已写、状态未存的崩溃不算撞号");
}

#[test]
fn copied_state_one_side_halts() {
    let (b, dek) = setup();
    let mut a = Dev::new("a", &dek);
    a.local.set("themes/t1.json", "1");
    a.sync(&b).unwrap();
    let mut a2 = Dev::new("a", &dek);
    a2.state = a.state.clone();
    a2.local = a.local.clone();

    a.local.set("themes/t2.json", "2");
    a.sync(&b).unwrap();
    a2.local.set("themes/t3.json", "3");
    let r2 = a2.sync(&b);
    let r1 = a.sync(&b);
    let hit = |r: &Result<SyncReport, SyncError>| matches!(r, Err(SyncError::DeviceIdConflict(_)));
    assert!(hit(&r1) || hit(&r2), "r1={r1:?} r2={r2:?}");
    let bad = if hit(&r1) { &mut a } else { &mut a2 };
    assert!(matches!(bad.sync(&b), Err(SyncError::Halted(_))));
}

#[test]
fn fresh_state_with_existing_remote_id_conflicts() {
    let (b, dek) = setup();
    let mut a = Dev::new("a", &dek);
    a.local.set("themes/t1.json", "1");
    a.sync(&b).unwrap();
    a.local.set("themes/t1.json", "2");
    a.sync(&b).unwrap();
    let mut fresh = Dev::new("a", &dek);
    assert!(matches!(
        fresh.sync(&b),
        Err(SyncError::DeviceIdConflict(_))
    ));
}

#[test]
fn fresh_state_first_publish_crash_recovers() {
    let (b, dek) = setup();
    let mut a = Dev::new("a", &dek);
    a.local.set("themes/t1.json", "1");
    let fresh = a.state.clone();
    a.sync(&b).unwrap();
    a.state = fresh;
    a.sync(&b).expect("首份清单写出后崩溃可恢复");
}
