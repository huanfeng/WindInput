//! 影子层（reverse-mode spec §4.2）：本方案**未启用**的扩展词库，只给码表的反查模式查询用（行内通配不读）。
//!
//! 代码里不叫 shadow——本仓「shadow」已专指候选调整（`apply_shadow` / `segment_shadow`），避免混淆。
//!
//! ★ 与引擎主 `DictManager` 完全分开：影子层自己持一个独立的 `DictManager`，**从不**注册进主 `dm`。
//! 普通打字（`convert`、活码探针、顶码、自动上屏复评）只查主 `dm`，于是未启用的库无论如何漏不进
//! 打字候选——这是「打字候选不受影响」的全部保证。
//!
//! 生命周期：
//! - 构造只登记来源（`declared`）与当前启用集，**不读盘**；
//! - [`DisabledDictLayers::warm`]（后台预热线程）或首次 [`DisabledDictLayers::search_pattern`]
//!   在锁内逐源加载「声明 − 启用」那些库，失败的 `warn!` 后跳过（不阻塞、不重试，直到下次失效）。
//!   两者经同一把锁串行：按键线程撞上正在进行的预热就等它建完、直接复用，不会重建第二遍；
//!   预热没赶上（或压根没起）时退化为按键线程现建，与没有预热时相同；
//! - 禁用一个启用中的库经 [`DisabledDictLayers::mark_disabled`] 进入影子集合、已加载的作废；
//!   启用方向不经这里——引擎整体失效重建，影子层随之重建。

use std::collections::HashSet;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use wind_candidate::Candidate;
use wind_dict::cached::CachedDict;
use wind_dict::{DictManager, SystemDictLayer};

/// 剪贴板反查小表的条目上限：超过就不建（大词库会让常驻内存数十 MB）。
const TEXT_CODES_MAX_ENTRIES: usize = 300_000;

/// 读一个扩展库的闭包：由构建方（`EngineManager`）按方案配置捕获路径 / 缓存参数。
pub type DictLoader = Arc<dyn Fn() -> anyhow::Result<CachedDict> + Send + Sync>;

/// 一个扩展词库来源（本方案 `[[dictionaries]]` 里的一条，不含主库）。
pub struct DisabledDictSource {
    /// 词库 id（与 `codetable-extra-<id>` 层名、`set_dict_enabled` 的 id 同口径）。
    pub id: String,
    /// `[[dictionaries]].base_order`，与它启用时挂进主 `dm` 的层一致。
    pub base_order: i32,
    /// `[[dictionaries]].default_weight`。
    pub default_weight: Option<i32>,
    pub load: DictLoader,
}

/// 文本 → 编码（按码长升序）的小表。
type TextCodes = Arc<std::collections::HashMap<String, Vec<String>>>;

/// 影子层：懒加载的未启用扩展词库集合。见模块文档。
pub struct DisabledDictLayers {
    /// 本方案**全部**扩展库来源（含已启用的——它们随时可能被禁用而进影子集合）。
    declared: Vec<DisabledDictSource>,
    /// 当前挂在引擎主 `dm` 上的扩展库 id。影子集合 = `declared − enabled`。
    enabled: Mutex<HashSet<String>>,
    wnorm: Option<wind_dict::WeightNorm>,
    /// 已加载的影子 `DictManager`；`None` = 尚未加载或已作废。
    loaded: Mutex<Option<Arc<DictManager>>>,
    /// 影子集合的「文本 → 编码（按码长升序）」小表，剪贴板反查用；随 `loaded` 一起作废。
    text_codes: Mutex<Option<TextCodes>>,
    /// 建过几遍影子 `DictManager`（测试与诊断用）。
    loads: AtomicUsize,
}

impl DisabledDictLayers {
    pub fn new(
        declared: Vec<DisabledDictSource>,
        enabled: impl IntoIterator<Item = String>,
        wnorm: Option<wind_dict::WeightNorm>,
    ) -> Self {
        Self {
            declared,
            enabled: Mutex::new(enabled.into_iter().collect()),
            wnorm,
            loaded: Mutex::new(None),
            text_codes: Mutex::new(None),
            loads: AtomicUsize::new(0),
        }
    }

    /// 通配查询影子集合（参数语义同 `DictManager::search_pattern`）。首次调用才加载。
    ///
    /// 返回的候选**不**置 `from_disabled_dict`——由引擎合并时（去重之后）置位。
    pub fn search_pattern(
        &self,
        pattern: &str,
        wildcard: char,
        limit: usize,
        with_prefix: bool,
    ) -> Vec<Candidate> {
        let dm = self.dm();
        dm.search_pattern(pattern, wildcard, limit, with_prefix)
    }

    /// 某个启用中的扩展库被禁用：它进影子集合，已加载的影子层作废（下次查询重建）。
    pub fn mark_disabled(&self, id: &str) {
        lock(&self.enabled).remove(id);
        *lock(&self.loaded) = None;
        *lock(&self.text_codes) = None;
    }

    /// 影子集合里 `text` 的全部编码（码长升序、去重），剪贴板反查（cmdbar `dict.rev`）用。
    /// 首次调用扫一遍影子层建小表（阻塞，**别放在按键线程首发**——预热线程会顺手建）。
    ///
    /// 先取 `text_codes` 锁、再取影子 `DictManager`，建表全程持着前一把：`mark_disabled` 清表时
    /// 会等到这里建完才清，不会出现「用旧的影子层建出的表写回、之后一直是旧数据」。
    /// 条目超过 [`TEXT_CODES_MAX_ENTRIES`] 的大词库不建表（常驻内存按条目线性增长），反查对
    /// 未启用库返回空。
    pub fn codes_of(&self, text: &str) -> Vec<String> {
        let map = {
            let mut g = lock(&self.text_codes);
            if g.is_none() {
                let dm = self.dm();
                let mut m: std::collections::HashMap<String, Vec<String>> = Default::default();
                let (mut n, mut too_big) = (0usize, false);
                dm.for_each_entry(&mut |code, t, _| {
                    if too_big {
                        return;
                    }
                    n += 1;
                    if n > TEXT_CODES_MAX_ENTRIES {
                        too_big = true;
                        return;
                    }
                    let v = m.entry(t.to_string()).or_default();
                    if !v.iter().any(|c| c == code) {
                        v.push(code.to_string());
                    }
                });
                if too_big {
                    tracing::warn!(
                        "未启用扩展词库条目超过 {TEXT_CODES_MAX_ENTRIES}，剪贴板反查不含它们"
                    );
                    m.clear();
                }
                for v in m.values_mut() {
                    v.sort_by_key(|c| c.len());
                }
                *g = Some(Arc::new(m));
            }
            g.as_ref().cloned()
        };
        map.and_then(|m| m.get(text).cloned()).unwrap_or_default()
    }

    /// 建过几遍影子 `DictManager`。
    pub fn load_count(&self) -> usize {
        self.loads.load(Ordering::SeqCst)
    }

    /// 预热：把影子集合现在就建好（阻塞读盘，**只可在后台线程调**）。返回是否真的建了——
    /// 已建好的直接返回 false。与查询共用 [`Self::dm`] 那把锁，故与按键线程天然串行。
    pub fn warm(&self) -> bool {
        let built = self.dm_built().1;
        // 反查小表也在后台建好，免得首次剪贴板反查在按键线程上扫表。
        let _ = self.codes_of("");
        built
    }

    /// 还没建、且此刻没有别的线程正在建 ⇒ 值得起一个预热线程。不阻塞（`try_lock`）：
    /// 锁被占着说明有人正在建（或正取 `Arc`），不必再起第二个。
    pub fn needs_warm(&self) -> bool {
        match self.loaded.try_lock() {
            Ok(g) => g.is_none(),
            Err(std::sync::TryLockError::Poisoned(e)) => e.into_inner().is_none(),
            Err(std::sync::TryLockError::WouldBlock) => false,
        }
    }

    /// 取已加载的影子 `DictManager`，没有就在锁内现建。查询在锁外做（只克隆 `Arc`）。
    fn dm(&self) -> Arc<DictManager> {
        self.dm_built().0
    }

    /// [`Self::dm`]，另带「这次是不是现建的」。
    fn dm_built(&self) -> (Arc<DictManager>, bool) {
        let mut loaded = lock(&self.loaded);
        if let Some(dm) = loaded.as_ref() {
            return (dm.clone(), false);
        }
        let enabled = lock(&self.enabled).clone();
        let dm = DictManager::new();
        for src in self.declared.iter().filter(|s| !enabled.contains(&s.id)) {
            match (src.load)() {
                Ok(dict) => dm.register_layer(Box::new(
                    SystemDictLayer::with_enabled(
                        dict,
                        format!("codetable-disabled-{}", src.id),
                        true,
                    )
                    .with_base_order(src.base_order)
                    .with_default_weight(src.default_weight)
                    .with_weight_norm(self.wnorm),
                )),
                Err(e) => tracing::warn!("未启用扩展词库 {} 加载失败，跳过：{e}", src.id),
            }
        }
        self.loads.fetch_add(1, Ordering::SeqCst);
        let dm = Arc::new(dm);
        *loaded = Some(dm.clone());
        (dm, true)
    }
}

/// 锁中毒时照用内部数据：这里的状态（id 集合 / 可重建的缓存）不存在「写了一半」的不变量。
fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use wind_dict::codetable::CodetableDict;

    fn mem_source(
        id: &str,
        entries: &[(&str, &str, i32)],
        calls: Arc<AtomicUsize>,
    ) -> DisabledDictSource {
        let owned: Vec<(String, String, i32)> = entries
            .iter()
            .map(|(c, t, w)| (c.to_string(), t.to_string(), *w))
            .collect();
        DisabledDictSource {
            id: id.into(),
            base_order: 3,
            default_weight: None,
            load: Arc::new(move || {
                calls.fetch_add(1, Ordering::SeqCst);
                let mut d = CodetableDict::empty();
                for (i, (c, t, w)) in owned.iter().enumerate() {
                    d.merge_single(c.clone(), t.clone(), *w, i as i32);
                }
                Ok(CachedDict::Memory(d))
            }),
        }
    }

    fn slot(p: &str) -> String {
        p.replace('?', &wind_dict::WILDCARD_SLOT.to_string())
    }

    /// spec §4.2：首次查询才加载，之后复用；启用集里的库不进影子。
    #[test]
    fn loads_lazily_once_and_skips_enabled() {
        let calls = Arc::new(AtomicUsize::new(0));
        let d = DisabledDictLayers::new(
            vec![
                mem_source("xz", &[("uuia", "门头沟区", 1)], calls.clone()),
                mem_source("ext", &[("uuib", "已启用库", 1)], calls.clone()),
            ],
            ["ext".to_string()],
            None,
        );
        assert_eq!(d.load_count(), 0, "构造不加载");
        let texts = |v: Vec<Candidate>| v.into_iter().map(|c| c.text).collect::<Vec<_>>();
        assert_eq!(
            texts(d.search_pattern(&slot("uui?"), wind_dict::WILDCARD_SLOT, 10, true)),
            ["门头沟区"]
        );
        d.search_pattern(&slot("uui?"), wind_dict::WILDCARD_SLOT, 10, true);
        assert_eq!(d.load_count(), 1, "第二次复用");
        assert_eq!(
            calls.load(Ordering::SeqCst),
            1,
            "只读了影子集合里的那一个库"
        );
    }

    /// 预热建一遍、之后查询复用；已建好时 `warm` 不再建，`needs_warm` 转 false。
    #[test]
    fn warm_builds_once_and_query_reuses_it() {
        let calls = Arc::new(AtomicUsize::new(0));
        let d = DisabledDictLayers::new(
            vec![mem_source("xz", &[("uuia", "门头沟区", 1)], calls.clone())],
            std::iter::empty(),
            None,
        );
        assert!(d.needs_warm());
        assert!(d.warm(), "首次预热真的建了");
        assert!(!d.needs_warm());
        assert!(!d.warm(), "已建好不再建");
        d.search_pattern(&slot("uui?"), wind_dict::WILDCARD_SLOT, 10, true);
        assert_eq!(d.load_count(), 1);
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        d.mark_disabled("xz");
        assert!(d.needs_warm(), "作废后需要重新预热");
    }

    /// 禁用一个库 ⇒ 它进影子集合、已加载的作废重建。
    #[test]
    fn mark_disabled_extends_set_and_drops_loaded() {
        let calls = Arc::new(AtomicUsize::new(0));
        let d = DisabledDictLayers::new(
            vec![
                mem_source("xz", &[("uuia", "门头沟区", 1)], calls.clone()),
                mem_source("ext", &[("uuib", "已启用库", 1)], calls.clone()),
            ],
            ["ext".to_string()],
            None,
        );
        d.search_pattern(&slot("uui?"), wind_dict::WILDCARD_SLOT, 10, true);
        d.mark_disabled("ext");
        let got: Vec<String> = d
            .search_pattern(&slot("uui?"), wind_dict::WILDCARD_SLOT, 10, true)
            .into_iter()
            .map(|c| c.text)
            .collect();
        assert!(
            got.contains(&"已启用库".to_string()) && got.contains(&"门头沟区".to_string()),
            "{got:?}"
        );
        assert_eq!(d.load_count(), 2);
    }

    /// 文件缺失 / 解析失败：跳过该库、warn，不影响其余库，也不 panic。
    #[test]
    fn failing_source_is_skipped() {
        let calls = Arc::new(AtomicUsize::new(0));
        let broken = DisabledDictSource {
            id: "gone".into(),
            base_order: 3,
            default_weight: None,
            load: Arc::new(|| anyhow::bail!("no such file")),
        };
        let d = DisabledDictLayers::new(
            vec![broken, mem_source("xz", &[("uuia", "门头沟区", 1)], calls)],
            std::iter::empty(),
            None,
        );
        let got = d.search_pattern(&slot("uui?"), wind_dict::WILDCARD_SLOT, 10, true);
        assert_eq!(got.len(), 1);
    }
}
