//! 方案常驻策略（`schema.keep_all_loaded`，设计 `docs/design/memory-footprint.md` §5）。
//!
//! - `true`：启动预热全部可用方案 + 临拼目标 + 英文，常驻不淘汰（此前的唯一行为）。
//! - `false`：启动只建用得着的（[`EngineManager::residency_protected`]：当前方案、临拼目标、
//!   临英开着时的英文；混输成员不保护，混输自建子引擎），其余可用方案在后台**只校验派生缓存**
//!   （[`EngineManager::refresh_schema_cache`]，过期即重建、建完即释放），切过去时现建；
//!   清扫拍子每分钟一次，30 分钟没用的方案引擎摘掉（[`EngineManager::evict_idle`]）。
//!
//! 「用过」的记录打在引擎侧取引擎的公共入口（`EngineManager::ensure_loaded`），本模块只管
//! 何时预热、何时清扫。30 分钟与 1 分钟不进配置（R1：「多久算不常用」程序能定）。
//!
//! 同一个清扫拍子还负责懒建缓存的闲置释放（设计 §7：单字全码表、按词查编码用户层），
//! 那部分不看常驻开关（[`EngineManager::evict_idle_caches`]）。
//!
//! [`EngineManager::evict_idle_caches`]: wind_engine::EngineManager::evict_idle_caches
//!
//! [`EngineManager::residency_protected`]: wind_engine::EngineManager::residency_protected
//! [`EngineManager::refresh_schema_cache`]: wind_engine::EngineManager::refresh_schema_cache
//! [`EngineManager::evict_idle`]: wind_engine::EngineManager::evict_idle

use std::time::Duration;

use tracing::{debug, info};

use crate::coordinator::Coordinator;

/// 方案引擎（`keep_all_loaded = false` 时）与懒建缓存（恒生效，设计 §7）多久没用算闲置。
#[cfg(feature = "desktop-ui")]
pub(crate) const IDLE_EVICT: Duration = Duration::from_secs(30 * 60);
/// 闲置清扫的拍长。
#[cfg(feature = "desktop-ui")]
pub(crate) const IDLE_SWEEP_TICK: Duration = Duration::from_secs(60);

impl Coordinator {
    /// 启动预热的方案部分（[`Self::prewarm_on_start`] 调；阻塞，只在后台线程 / 测试里调）。
    pub(crate) fn prewarm_schemas_on_start(&self) {
        if self.rt().config.schema.keep_all_loaded {
            self.prewarm_all_schemas();
        } else {
            self.prewarm_resident_schemas();
        }
    }

    /// `keep_all_loaded = true`：建好 `available` 全部方案 + 临拼 / 临英目标的引擎。
    pub(crate) fn prewarm_all_schemas(&self) {
        let active = self.engine_mgr.active_schema_id();
        // available_schemas 只含「可切换的方案」。临时拼音 / 临时英文的目标引擎
        // **不在其中**（它们是模式的实现，不是可切换方案），此前因此漏出预热范围：
        // 实测首次按引导键进临拼时才同步加载 52 万词条的拼音库 + 英文库，用户感到
        // 顿一下。两者都只在启用时才预热，不给没开这些功能的用户白付内存。
        let mut targets: Vec<String> = self.engine_mgr.available_schemas().to_vec();
        // ⚠ `temp_pinyin_target()` **自身就会 `ensure_loaded`**（它的语义是「可用才
        // 返回」），故这一行本身即完成了临拼引擎的加载，下面循环里那次只是复查跳过。
        // 看着绕，但比在此复制一份「开关 + 方案适用性 + 目标解析」的判据强——那套判据
        // 是所有临拼入口的公共门卫，抄一份必然漂移。
        if let Some(t) = self.engine_mgr.temp_pinyin_target() {
            targets.push(t);
        }
        if self.rt().config.input.temp_english.show_candidates {
            targets.push("english".to_string());
        }
        for id in targets {
            // 逐个复核：预热途中用户把常驻关掉了，就别再往里建（已建的交给淘汰）。
            if !self.rt().config.schema.keep_all_loaded {
                debug!("常驻已关，停止预热");
                break;
            }
            if id == active || self.engine_mgr.is_loaded(&id) {
                continue;
            }
            let t0 = std::time::Instant::now();
            if self.engine_mgr.prewarm_schema(&id) {
                debug!("Prewarmed schema {} in {:?}", id, t0.elapsed());
            } else {
                debug!("Prewarm skipped/failed for schema {}", id);
            }
        }
        debug!("Schema prewarm done");
    }

    /// `keep_all_loaded = false`：只建常驻集合（当前方案已在构造时建好）。
    fn prewarm_resident_schemas(&self) {
        // 与 `prewarm_all_schemas` 同一个门卫（它自己会 ensure_loaded）。
        let _ = self.engine_mgr.temp_pinyin_target();
        if self.rt().config.input.temp_english.show_candidates {
            let _ = self.engine_mgr.prewarm_schema("english");
        }
    }

    /// `keep_all_loaded = false` 时，校验其余可用方案的派生缓存（过期 / 缺失即重建），
    /// 建完即释放引擎（[`Self::prewarm_on_start`] 在反查索引预热之后调；阻塞）。
    ///
    /// 必须在启动后台做，不能留到用户切换那一刻：缓存过期时重建是秒级（雾凇 merged 实测
    /// 4 秒、峰值 491 MB），挪到这里之后切换只剩 mmap + 建外壳。排在反查索引之后：那是
    /// 打字马上要用的，这一步只为以后的切换。构建期峰值是一次性的，校验完整理一次堆落回。
    pub(crate) fn refresh_schema_caches_on_start(&self) {
        if self.rt().config.schema.keep_all_loaded {
            return;
        }
        let t0 = std::time::Instant::now();
        let mut checked = 0usize;
        for id in self.engine_mgr.available_schemas() {
            if self.engine_mgr.is_loaded(&id) {
                continue;
            }
            if self.engine_mgr.refresh_schema_cache(&id) {
                checked += 1;
            } else {
                debug!("缓存校验跳过/失败：{}", id);
            }
        }
        // 校验时混输取走了共享英文引擎；眼下若没有消费者，让缓存放手，英文不再受保护。
        self.engine_mgr.release_shared_english_if_unused();
        crate::heap_trim::release_free_heap();
        info!(
            "方案按需加载：常驻 {:?}，校验 {} 个方案的词库缓存用时 {:?}",
            self.engine_mgr.loaded_schemas(),
            checked,
            t0.elapsed()
        );
    }

    /// 清扫拍子的主体：
    ///
    /// - 懒建缓存（单字全码表、按词查编码用户层，设计 §7）闲置 `idle` 以上就释放——**恒生效**，
    ///   与常驻策略无关（[`wind_engine::EngineManager::evict_idle_caches`]）；
    /// - 常驻关着时再摘掉闲置 `idle` 以上的方案引擎。
    ///
    /// 放掉了东西就整理一次堆（释放只是回到分配器手里）。返回被摘的方案 id。
    pub(crate) fn idle_sweep(&self, idle: Duration) -> Vec<String> {
        let released = self.engine_mgr.evict_idle_caches(idle);
        let evicted = if self.rt().config.schema.keep_all_loaded {
            Vec::new()
        } else {
            self.engine_mgr.evict_idle(idle)
        };
        if !evicted.is_empty() || !released.is_empty() {
            crate::heap_trim::release_free_heap();
        }
        evicted
    }

    /// 生产构造器在 `build` 之后起闲置清扫线程（每 [`IDLE_SWEEP_TICK`] 一拍）。
    ///
    /// 单起一条而不是挂在已有线程上：协调器现有的后台线程要么默认挂起（全屏复查只在工具栏
    /// 显示时醒）、要么属于别的 crate（redb 的空闲回收在 wind-store，store 还可能不存在）。
    /// 一分钟醒一次的成本可忽略；线程只持 `Weak`，协调器析构后下一拍自行退出。
    /// 以后的其它闲置清扫（设计 §7）接在 [`Self::idle_sweep`] 上，不再另起线程。
    #[cfg(feature = "desktop-ui")]
    pub(crate) fn spawn_idle_sweeper(&self) {
        let Some(weak) = self.self_weak.get().cloned() else {
            return;
        };
        let spawned = std::thread::Builder::new()
            .name("idle-sweep".into())
            .spawn(move || {
                loop {
                    std::thread::sleep(IDLE_SWEEP_TICK);
                    let Some(c) = weak.upgrade() else {
                        break;
                    };
                    c.idle_sweep(IDLE_EVICT);
                }
            });
        if let Err(e) = spawned {
            tracing::warn!("无法启动闲置清扫线程: {e}（方案引擎不会自动卸载）");
        }
    }

    /// 配置重载时 `schema.keep_all_loaded` 变了：立即按新策略对齐，不需要重启。
    ///
    /// - true → false：保护名单外的引擎立即摘掉（`idle = 0`）。
    /// - false → true：后台预热全部（与启动走同一个 [`Self::prewarm_schemas_on_start`]，
    ///   此时它读到的已是新配置）。
    ///
    /// 这个键刻意不进 `engine_reload_needed`：改它不该把全部引擎丢掉重建。
    pub(crate) fn apply_residency_change(&self, was_keep_all: bool) {
        let keep_all = self.rt().config.schema.keep_all_loaded;
        if keep_all == was_keep_all {
            return;
        }
        if keep_all {
            // 宿主声明按需加载（移动端 `set_eager_prewarm(false)`）时，同启动一样不建。
            if !self
                .eager_prewarm
                .load(std::sync::atomic::Ordering::Relaxed)
            {
                return;
            }
            let Some(weak) = self.self_weak.get().cloned() else {
                return;
            };
            let _ = std::thread::Builder::new()
                .name("residency-prewarm".into())
                .spawn(move || {
                    if let Some(c) = weak.upgrade() {
                        c.prewarm_schemas_on_start();
                    }
                });
        } else {
            if !self.engine_mgr.evict_idle(Duration::ZERO).is_empty() {
                crate::heap_trim::release_free_heap();
            }
        }
    }
}
