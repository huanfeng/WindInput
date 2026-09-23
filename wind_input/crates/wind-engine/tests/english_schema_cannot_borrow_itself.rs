//! 英文方案不能把自己当成自己的英文子引擎——否则是**死锁**，不是逻辑错。
//!
//! `read_schema` 合并 override 走的是无白名单的 `merge_toml`，所以用户（或一个导入的方案包）
//! 在 `english.toml` 里写一句 `[engine] type = "mixed"`，英文方案就变成了混输。此时：
//!
//! ```text
//! shared_english_engine() → ensure_loaded("english") → 持 build_locks["english"]
//!   → build_engine("english", …, Some(provider)) → 走 mixed 分支
//!     → provider() = shared_english_engine() → 缓存仍空（上一层还没写回）
//!       → ensure_loaded("english") → 再取 build_locks["english"]
//! ```
//!
//! `std::sync::Mutex` 不可重入，同一线程第二次 `lock()` 就是挂死：整条打字线路停住，
//! 用户只能杀进程。所以本用例的判据是**能不能跑完**，不是结果对不对。
//!
//! 修法是构建 `ENGLISH_SCHEMA` 自身时不传 `english_provider`，退化成「这个畸形的混输拿不到
//! 英文子引擎」——少一路候选，但活着。
//!
//! ⚠️ 词库缺失时静默跳过（同 `english_engine_is_shared.rs` 的约定）。

use std::path::PathBuf;
use std::sync::mpsc;
use std::time::Duration;
use wind_config::Config;
use wind_engine::EngineManager;

fn data_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../build_dev/data")
}

/// ★ 把英文方案的 override 改成 `type = "mixed"`，加载它必须**返回**（而不是挂死）。
///
/// 判据落在超时上：死锁下这个线程永远不会给出结果，`recv_timeout` 是唯一能把「挂死」
/// 变成可观测失败的办法。回归时本用例的表现是超时红，不是断言红。
///
/// 反向验证（变异）：把 `ensure_loaded` 里那个 `if schema_id == ENGLISH_SCHEMA { None }`
/// 换回无条件的 `Some(&|| self.shared_english_engine())`，本用例立刻超时红（实跑确认）。
#[test]
fn a_malformed_english_schema_typed_as_mixed_must_not_deadlock() {
    let dir = data_dir();
    if !dir.join("schemas/english.schema.toml").exists() {
        eprintln!("跳过：缺少英文方案");
        return;
    }

    let ov = std::env::temp_dir().join("wind_en_self_borrow_ov");
    let _ = std::fs::remove_dir_all(&ov);
    std::fs::create_dir_all(&ov).unwrap();
    // 先把畸形 override 落盘，再建 manager——让它从一开始就读到 type = "mixed"。
    std::fs::write(
        ov.join("english.toml"),
        "[engine]\ntype = \"mixed\"\n\n[engine.mixed]\nprimary_schema = \"wubi86\"\n",
    )
    .unwrap();

    let mut cfg = Config::default();
    cfg.schema.available = vec!["english".into()];
    cfg.schema.active = "english".into();
    // ★ 必须开着：`enable_english` 关着时 mixed 分支压根不求值那个闭包，死锁不会发生，
    //   本用例就退化成恒绿。
    cfg.schema.mix.enable_english = true;

    let (tx, rx) = mpsc::channel();
    let handle = std::thread::spawn(move || {
        let mgr = EngineManager::with_store_override(&cfg, Some(&data_dir()), None, Some(ov));
        let loaded = mgr.ensure_schema("english");
        let _ = tx.send(loaded);
    });

    match rx.recv_timeout(Duration::from_secs(60)) {
        Ok(_) => {
            handle.join().expect("构建线程不该 panic");
        }
        Err(_) => panic!(
            "加载被写成 mixed 的英文方案挂住了 60 秒 —— 这就是 build_locks[\"english\"] \
             的自重入死锁。英文方案构建时不许再经 english_provider 借自己。"
        ),
    }
}
