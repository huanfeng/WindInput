//! `state.toml` 的**单点延迟写入器**：合并短时间内的多次写，并且把整个进程的
//! load-modify-save 收敛到一条线程上。
//!
//! # 为什么要有它
//!
//! 两个问题各自都不致命，叠在一起才难查：
//!
//! 1. **互相吞更新**。此前每个写入点各自 `RuntimeState::load` → 改一个字段 → `save`。
//!    两处几乎同时写（拖工具栏 + 关软键盘记住当前面）时，后写的那次读到的是**改动前**
//!    的快照，落盘时把先写的那次原样覆盖回去。`handle_softkeyboard::save_softkeyboard_page`
//!    的注释早就点出过这个形态（"一次丢更新就能吞掉刚存好的 toolbar_positions"），
//!    但当时只在**测试与生产之间**加了道门，进程内多个写入点之间的竞争仍在。
//!    收到一条线程上之后，"读—改—写"整体串行，这类丢更新在进程内不再可能。
//! 2. **写得太碎**。位置类状态天然会被连续微调（拖一下、看一眼、再拖一下），
//!    每次都是一轮完整的读文件 + 序列化 + 写临时文件 + rename。
//!
//! # 合并语义：按「变更种类」覆盖，不是按次数排队
//!
//! [`StateWriter::schedule`] 的 `kind` 是变更种类（`"toolbar_anchors"` 等）。同一种类
//! 的后一次请求**覆盖**前一次待落的那次（位置类状态只有最后一次有意义），不同种类
//! 并存。故待落队列的长度恒等于种类数，不随用户拖多少次增长。
//!
//! ⚠️ 正因为是覆盖语义，`schedule` 的闭包必须**自带完整的目标值**（从调用方的内存
//! 镜像整份克隆过来），不能写成"在旧值上增量修改"——被覆盖掉的那次增量会凭空消失。
//!
//! # 关机时必须 flush
//!
//! 防抖窗口内进程退出就会丢掉最后一次改动，而"最后一次"恰恰是用户最在意的那次
//! （拖完就关机）。[`StateWriter`] 的 `Drop` 会把待落变更写完再返回，
//! 见 [`StateWriter::flush_blocking`]。

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use tracing::warn;
use wind_config::RuntimeState;

/// 合并窗口：末次请求之后再等这么久才真正落盘。
///
/// 取 1s 的判据是"比人连续微调的间隔长、比人拖完就关机的反应时间短"。调大能进一步
/// 减少写盘，但会让 `Drop` 里那次 flush 承担更多"本该早就写掉"的变更——而进程被强杀
/// （任务管理器、安装程序升级）时 `Drop` 根本不跑，那些变更就真的丢了。
const DEBOUNCE: Duration = Duration::from_millis(1000);

/// ⚠️ 是 `Fn` 而不是 `FnOnce`：写失败要能**原样重放**（见 `run` 的重试）。
/// 故闭包内取用捕获值时要 `clone`，不能 move 走。
type Edit = Box<dyn Fn(&mut RuntimeState) + Send>;

struct Pending {
    /// 待落变更，按种类去重（同种覆盖）。`BTreeMap` 而非 `HashMap`：种类数是个位数，
    /// 有序遍历让"同一批变更的落盘顺序"可复现，出问题时日志能对得上。
    edits: BTreeMap<&'static str, Edit>,
    /// 最早可以落盘的时刻；`None` = 没有待落变更。
    due: Option<Instant>,
    /// 已请求停止：worker 把剩余变更写完就退出。
    stopping: bool,
}

struct Inner {
    state_dir: PathBuf,
    pending: Mutex<Pending>,
    cv: Condvar,
}

/// `state.toml` 延迟写入器。克隆共享同一条 worker 线程。
pub(crate) struct StateWriter {
    /// `None` = 本实例不写盘（无 `store` 的测试夹具，或取不到状态目录）。
    ///
    /// 判据刻意保留在**构造方**：`store.is_none()` 表示"headless 测试夹具"这个语义不是
    /// 这里赋予的，而 `state_dir()` 是进程外的全局路径（`%LOCALAPPDATA%\WindInput[Dev]`），
    /// 测试夹具同样取得到——只靠路径判断挡不住"跑一次 cargo test 就改掉开发者本机状态、
    /// 还与正在运行的服务抢同一个文件"。
    inner: Option<Arc<Inner>>,
    worker: Option<std::thread::JoinHandle<()>>,
}

impl StateWriter {
    /// `enabled = false` 或 `state_dir = None` 时构造出一个**不写盘的空实现**：
    /// `schedule` 变成 no-op，不起线程。
    pub(crate) fn new(enabled: bool, state_dir: Option<PathBuf>) -> Self {
        let Some(dir) = state_dir.filter(|_| enabled) else {
            return Self {
                inner: None,
                worker: None,
            };
        };
        let inner = Arc::new(Inner {
            state_dir: dir,
            pending: Mutex::new(Pending {
                edits: BTreeMap::new(),
                due: None,
                stopping: false,
            }),
            cv: Condvar::new(),
        });
        let w = Arc::clone(&inner);
        let worker = std::thread::Builder::new()
            .name("state-writer".into())
            .spawn(move || run(&w))
            .ok();
        Self {
            inner: Some(inner),
            worker,
        }
    }

    /// 本实例是否是**不写盘的空实现**（无 `store` 的测试夹具 / 取不到状态目录）。
    ///
    /// 供测试直接断言"这个协调器没有落盘能力"。⚠️ 别用副作用（某个内存镜像有没有变）
    /// 去代理这件事——那种判据会在写入路径重构时静默失效，而失败信息还会指向错误的
    /// 结论（"测试正在改开发者本机的 state.toml"，实际并没有）。
    #[cfg(test)]
    pub(crate) fn is_noop(&self) -> bool {
        self.inner.is_none()
    }

    /// 登记一次变更：`kind` 相同的前一次待落变更被本次覆盖。
    ///
    /// `edit` 会在 worker 线程上、拿着**刚从磁盘读出来的** [`RuntimeState`] 执行，
    /// 所以它必须只改自己那几个字段、把值整份写进去（见模块文档的覆盖语义警告）。
    pub(crate) fn schedule(
        &self,
        kind: &'static str,
        edit: impl Fn(&mut RuntimeState) + Send + 'static,
    ) {
        let Some(inner) = &self.inner else {
            return;
        };
        let mut p = inner.pending.lock().unwrap_or_else(|e| e.into_inner());
        if p.stopping {
            return;
        }
        p.edits.insert(kind, Box::new(edit));
        // 每次新请求都把到期时刻推后 = 末次触发防抖（连续微调只写最后一次）。
        p.due = Some(Instant::now() + DEBOUNCE);
        inner.cv.notify_all();
    }

    /// 把待落变更立刻写完并停掉 worker。幂等；`Drop` 会调用它。
    ///
    /// **不带超时**：这里要等的是一次本地小文件写入，而漏写的代价是用户刚拖好的位置
    /// 凭空丢失。若哪天 worker 真的可能卡住（写入换成网络路径之类），该加的是那一侧的
    /// 超时，而不是在这里放弃等待——那只会把丢数据变成偶发。
    fn flush_blocking(&mut self) {
        let Some(inner) = self.inner.take() else {
            return;
        };
        {
            let mut p = inner.pending.lock().unwrap_or_else(|e| e.into_inner());
            p.stopping = true;
            // 已登记的变更立即到期，不再等防抖窗口。
            if !p.edits.is_empty() {
                p.due = Some(Instant::now());
            }
        }
        inner.cv.notify_all();
        if let Some(h) = self.worker.take() {
            let _ = h.join();
        }
    }
}

impl Drop for StateWriter {
    fn drop(&mut self) {
        self.flush_blocking();
    }
}

/// 写失败后的重放次数上限。
///
/// `state.toml` 偶发写不进去是有真实来源的：同步盘/备份工具短暂持有文件句柄、
/// 杀毒软件扫描、磁盘瞬时满。这类都是**几秒内自愈**的，重放一两次就过去了；
/// 而真正的持久故障（目录被删、只读介质）重试多少次都没用，只会刷日志。
const MAX_RETRIES: u32 = 3;

fn run(inner: &Arc<Inner>) {
    let mut retries: u32 = 0;
    loop {
        let batch = {
            let mut p = inner.pending.lock().unwrap_or_else(|e| e.into_inner());
            loop {
                if p.edits.is_empty() {
                    if p.stopping {
                        return;
                    }
                    p = inner.cv.wait(p).unwrap_or_else(|e| e.into_inner());
                    continue;
                }
                let now = Instant::now();
                match p.due {
                    // 还没到期：等到期，期间若有新请求会把 due 推后，醒来重新判断。
                    Some(due) if due > now => {
                        let (g, _) = inner
                            .cv
                            .wait_timeout(p, due - now)
                            .unwrap_or_else(|e| e.into_inner());
                        p = g;
                        continue;
                    }
                    _ => {}
                }
                p.due = None;
                break std::mem::take(&mut p.edits);
            }
        };
        // ⚠️ 锁已释放再做文件 IO：`schedule` 在任意线程上调用（UI 事件线程、
        // bridge 线程），不该被一次磁盘写入阻塞。
        match apply(&inner.state_dir, &batch) {
            Ok(()) => retries = 0,
            Err(e) => {
                if retries >= MAX_RETRIES {
                    warn!(
                        "state.toml 保存失败，已重试 {retries} 次，放弃本批 ({}): {e}",
                        batch.keys().copied().collect::<Vec<_>>().join("+")
                    );
                    retries = 0;
                    continue;
                }
                retries += 1;
                // 原样放回重排。⚠️ 期间若有同 kind 的新值进来，**保留新值**——
                // 位置类状态只有最后一次有意义，拿旧值盖回去等于把用户刚拖到的位置
                // 退回上一处，比这次没写成还糟。
                let mut p = inner.pending.lock().unwrap_or_else(|e| e.into_inner());
                for (kind, edit) in batch {
                    p.edits.entry(kind).or_insert(edit);
                }
                // 停止流程中不等防抖窗口：`flush_blocking` 正 join 着这条线程。
                p.due = Some(if p.stopping {
                    Instant::now()
                } else {
                    Instant::now() + DEBOUNCE
                });
            }
        }
    }
}

fn apply(
    state_dir: &std::path::Path,
    edits: &BTreeMap<&'static str, Edit>,
) -> Result<(), std::io::Error> {
    if edits.is_empty() {
        return Ok(());
    }
    let mut rs = RuntimeState::load(state_dir);
    for edit in edits.values() {
        edit(&mut rs);
    }
    rs.save(state_dir)
        .map_err(|e| std::io::Error::other(e.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 空实现：不起线程、`schedule` 不写盘，也不 panic。
    ///
    /// 这条钉的是测试夹具的安全性——协调器的单测会构造出成百上千个实例，
    /// 任何一个真写了盘，跑一次 `cargo test` 就改掉开发者本机的 `state.toml`，
    /// 还会与正在运行的服务抢同一个文件。
    #[test]
    fn disabled_writer_is_a_noop() {
        let dir = std::env::temp_dir().join("wind-state-writer-noop-must-not-exist");
        let w = StateWriter::new(false, Some(dir.clone()));
        w.schedule("toolbar_anchors", |rs| {
            rs.toolbar_anchors.insert("k".into(), (1, 2));
        });
        drop(w);
        assert!(
            !dir.join("state.toml").exists(),
            "禁用的写入器不得落盘: {}",
            dir.display()
        );
    }

    /// 同种类的多次 `schedule` 只落**最后一次**；不同种类并存。
    ///
    /// 前半条是防抖的目的本身（连续微调不该写 N 次盘），后半条是它的边界：
    /// 合并绝不能把别的字段一起吞掉——那正是本模块要根治的"丢更新"。
    #[test]
    fn same_kind_collapses_and_other_kinds_survive() {
        let dir = tempdir();
        {
            let w = StateWriter::new(true, Some(dir.clone()));
            for i in 1..=5 {
                w.schedule("toolbar_anchors", move |rs| {
                    rs.toolbar_anchors.insert("m".into(), (i, i));
                });
            }
            w.schedule("softkeyboard_anchors", |rs| {
                rs.softkeyboard_anchors.insert("m".into(), (9, 9));
            });
            // Drop 里 flush：不必等满一个防抖窗口。
        }
        let rs = RuntimeState::load(&dir);
        assert_eq!(rs.toolbar_anchors.get("m"), Some(&(5, 5)), "该留最后一次");
        assert_eq!(
            rs.softkeyboard_anchors.get("m"),
            Some(&(9, 9)),
            "异种不得被吞"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 关机 flush：`Drop` 返回时防抖窗口内的变更必须已经落盘。
    ///
    /// 没有这一条，"拖完立刻关机"就丢——而那恰恰是用户最在意的那一次拖动。
    #[test]
    fn drop_flushes_pending_edit() {
        let dir = tempdir();
        {
            let w = StateWriter::new(true, Some(dir.clone()));
            w.schedule("toolbar_anchors", |rs| {
                rs.toolbar_anchors.insert("bye".into(), (7, 8));
            });
        }
        assert_eq!(
            RuntimeState::load(&dir).toolbar_anchors.get("bye"),
            Some(&(7, 8)),
            "Drop 返回时待落变更应已写盘"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 多次 flush（`Drop` 之外还手动调过）不得 panic 或重复 join。
    #[test]
    fn flush_is_idempotent() {
        let dir = tempdir();
        let mut w = StateWriter::new(true, Some(dir.clone()));
        w.schedule("toolbar_anchors", |rs| {
            rs.toolbar_anchors.insert("x".into(), (1, 1));
        });
        w.flush_blocking();
        w.flush_blocking();
        drop(w);
        assert_eq!(
            RuntimeState::load(&dir).toolbar_anchors.get("x"),
            Some(&(1, 1))
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 停止之后再来的 `schedule` 被丢弃，而不是让 worker 永远等下去。
    #[test]
    fn schedule_after_stop_is_dropped() {
        let dir = tempdir();
        let mut w = StateWriter::new(true, Some(dir.clone()));
        w.flush_blocking();
        w.schedule("toolbar_anchors", |rs| {
            rs.toolbar_anchors.insert("late".into(), (3, 3));
        });
        drop(w);
        assert!(
            !RuntimeState::load(&dir)
                .toolbar_anchors
                .contains_key("late"),
            "停止后的变更不该落盘"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn tempdir() -> PathBuf {
        let d = std::env::temp_dir().join(format!(
            "wind-state-writer-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).expect("建临时目录");
        d
    }
}
