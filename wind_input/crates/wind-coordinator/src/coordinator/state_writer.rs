//! `state.toml` 的单点延迟写入器：合并短时间内的多次写，并把整个进程的
//! load-modify-save 收敛到一条线程上。
//!
//! 解决两个问题：各写入点各自「读—改—写」时并发会互相吞更新（后写的读到旧快照、覆盖先写
//! 的）；位置类状态被连续微调时写得太碎。收到一条线程后进程内不再丢更新。
//!
//! # 合并语义：按「变更种类」覆盖，不是按次数排队
//!
//! [`StateWriter::schedule`] 的 `kind` 是变更种类（`"toolbar_anchors"` 等），同种类后一次
//! 覆盖前一次待落的，不同种类并存，队列长度恒等于种类数。正因为是覆盖语义，闭包必须自带
//! 完整目标值（从内存镜像整份克隆），不能写成增量修改——被覆盖的那次增量会凭空消失。
//!
//! # 关机时必须 flush
//!
//! 防抖窗口内退出会丢掉最后一次改动（拖完就关机）。`Drop` 会把待落变更写完再返回，
//! 见 [`StateWriter::flush_blocking`]。

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use tracing::warn;
use wind_config::RuntimeState;

/// 合并窗口：末次请求之后再等这么久才落盘。比人连续微调的间隔长、比拖完就关机的反应
/// 时间短；调大会让更多变更压到 `Drop` 的 flush 上，而进程被强杀时 `Drop` 不跑。
const DEBOUNCE: Duration = Duration::from_millis(1000);

/// 是 `Fn` 而不是 `FnOnce`：写失败要能原样重放（见 `run` 的重试）。
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
    /// 判据刻意由构造方给：`state_dir()` 是进程外的全局路径，测试夹具同样取得到，只靠路径
    /// 判断挡不住 cargo test 改掉开发者本机状态、与运行中的服务抢同一个文件。
    inner: Option<Arc<Inner>>,
    worker: Option<std::thread::JoinHandle<()>>,
}

impl StateWriter {
    /// `enabled = false` 或 `state_dir = None` 时构造出一个不写盘的空实现：
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

    /// 本实例是否是不写盘的空实现（无 `store` 的测试夹具 / 取不到状态目录）。
    /// 供测试直接断言；别用「某个内存镜像变没变」代理，那种判据会在重构时静默失效。
    #[cfg(test)]
    pub(crate) fn is_noop(&self) -> bool {
        self.inner.is_none()
    }

    /// 登记一次变更：`kind` 相同的前一次待落变更被本次覆盖。
    ///
    /// `edit` 会在 worker 线程上、拿着刚从磁盘读出来的 [`RuntimeState`] 执行，
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
    /// 不带超时：漏写的代价是用户刚拖好的位置丢失。worker 若真可能卡住，该在那一侧加超时。
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

/// 写失败后的重放次数上限。偶发失败（同步盘/杀毒短暂持有句柄、磁盘瞬时满）几秒内自愈，
/// 重放一两次就过；持久故障重试多少次都没用，只会刷日志。
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
        // 锁已释放再做文件 IO：`schedule` 在任意线程上调用（UI 事件线程、
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
                // 原样放回重排。期间若有同 kind 的新值进来，保留新值——
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
#[path = "state_writer_tests.rs"]
mod tests;
