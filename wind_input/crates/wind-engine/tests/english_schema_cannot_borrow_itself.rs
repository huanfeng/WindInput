//! 英文方案不能把自己当成自己的英文子引擎，否则是死锁，不是逻辑错。
//!
//! `read_schema` 合并 override 走无白名单的 `merge_toml`，用户（或导入的方案包）在 `english.toml`
//! 里写一句 `[engine] type = "mixed"`，英文方案就变成了混输。此时：
//!
//! ```text
//! shared_english_engine() → ensure_loaded("english") → 持 build_locks["english"]
//!   → build_engine("english", …, Some(provider)) → 走 mixed 分支
//!     → provider() = shared_english_engine() → 缓存仍空（上一层还没写回）
//!       → ensure_loaded("english") → 再取 build_locks["english"]
//! ```
//!
//! `std::sync::Mutex` 不可重入，同一线程第二次 `lock()` 就挂死整条打字线路。所以判据是能不能跑完。
//! 修法是构建 `ENGLISH_SCHEMA` 自身时不传 `english_provider`：畸形混输少一路候选，但活着。
//!
//! 词库缺失时静默跳过（同 `english_engine_is_shared.rs` 的约定）。

use std::path::PathBuf;
use std::sync::mpsc;
use std::time::Duration;
use wind_config::Config;
use wind_engine::EngineManager;

fn data_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../build_dev/data")
}

/// 把英文方案的 override 改成 `type = "mixed"`，加载它必须返回而不是挂死。
///
/// 死锁下线程永远不给结果，`recv_timeout` 把挂死变成可观测失败，回归时是超时红。变异验证：把
/// `ensure_loaded` 里 `if schema_id == ENGLISH_SCHEMA { None }` 换回无条件 `Some(...)` 即超时红。
#[test]
fn a_malformed_english_schema_typed_as_mixed_must_not_deadlock() {
    let dir = data_dir();
    if !dir.join("schemas/english.schema.toml").exists() {
        eprintln!("跳过：缺少英文方案");
        return;
    }

    // 目录名带进程号：并发会话同时跑本用例会互相删目录，english.toml 被删掉后 english 按
    // 普通方案加载，用例恒绿却什么也没测到。
    let ov = std::env::temp_dir().join(format!("wind_en_self_borrow_ov_{}", std::process::id()));
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
    // 必须开着：`enable_english` 关着时 mixed 分支不求值那个闭包，死锁不会发生，本用例退化成恒绿。
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
