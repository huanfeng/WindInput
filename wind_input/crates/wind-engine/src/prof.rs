//! 评测专用的分阶段计时（feature `aux-prof`；`aux-prof-fine` 另在辅码校验的逐字判定里取 TSC）。
//!
//! 未开 feature 时 [`span`] / [`count`] / [`fine`] 都是空操作、[`take`] 恒空，不进生产热路径。
//! 计数按线程累加，评测工具在一遍计时前后 [`take`] 取走。设计见
//! `docs/design/aux-code-continuous.md` 附录「P3 耗时」。

/// 计时阶段。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(usize)]
pub enum Stage {
    /// `raw_base_lattice` 整体
    Base,
    /// 读法展开（`spans_from`）
    Spans,
    /// 查词（`search_with_boundary`）
    Search,
    /// 不带辅码读法的终点上逐条命中建节点
    HitsPlain,
    /// 带辅码读法的终点上逐条命中校验、建节点
    HitsAux,
    /// 辅码校验：取字（`chars().count` / `nth`）—— 仅 `aux-prof-fine`
    FineNth,
    /// 辅码校验：`(字, 前缀)` 缓存查找 —— 仅 `aux-prof-fine`
    FineCache,
    /// 辅码校验：trait 调用（缓存未命中）—— 仅 `aux-prof-fine`
    FineTrait,
    /// 多字残码补全
    TailCompletion,
    /// 用户词进图
    Store,
    /// 滤屏蔽词、末字辅码表、转解码节点
    ToWordNodes,
    /// step 2 整句解码
    Step2Decode,
    /// S_n 整串整句解码
    SnDecode,
    /// 闸门对手：去辅码底图（`plain_lattice`）
    GatePlainLattice,
    /// 闸门对手解码
    GateDecode,
    /// 2c 残码整句（建图 + 解码）
    Step2c,
}

impl Stage {
    pub const ALL: [Stage; 16] = [
        Stage::Base,
        Stage::Spans,
        Stage::Search,
        Stage::HitsPlain,
        Stage::HitsAux,
        Stage::FineNth,
        Stage::FineCache,
        Stage::FineTrait,
        Stage::TailCompletion,
        Stage::Store,
        Stage::ToWordNodes,
        Stage::Step2Decode,
        Stage::SnDecode,
        Stage::GatePlainLattice,
        Stage::GateDecode,
        Stage::Step2c,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Stage::Base => "base",
            Stage::Spans => "spans",
            Stage::Search => "search",
            Stage::HitsPlain => "hits_plain",
            Stage::HitsAux => "hits_aux",
            Stage::FineNth => "fine_nth",
            Stage::FineCache => "fine_cache",
            Stage::FineTrait => "fine_trait",
            Stage::TailCompletion => "tail_completion",
            Stage::Store => "store",
            Stage::ToWordNodes => "to_word_nodes",
            Stage::Step2Decode => "step2_decode",
            Stage::SnDecode => "sn_decode",
            Stage::GatePlainLattice => "gate_plain_lattice",
            Stage::GateDecode => "gate_decode",
            Stage::Step2c => "step2c",
        }
    }
}

/// 计数项。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(usize)]
pub enum Counter {
    /// 带辅码读法的终点上遍历的命中条数
    AuxHits,
    /// `admits` 调用次数（带辅码的读法）
    Admits,
    /// `admits` 放行次数
    Admitted,
    /// `(字, 前缀)` 判定次数
    CharChecks,
    /// 其中缓存未命中（调 trait）
    TraitCalls,
}

impl Counter {
    pub const ALL: [Counter; 5] = [
        Counter::AuxHits,
        Counter::Admits,
        Counter::Admitted,
        Counter::CharChecks,
        Counter::TraitCalls,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Counter::AuxHits => "aux_hits",
            Counter::Admits => "admits",
            Counter::Admitted => "admitted",
            Counter::CharChecks => "char_checks",
            Counter::TraitCalls => "trait_calls",
        }
    }
}

/// 一次 [`take`] 的结果：各阶段纳秒数与各计数（按 `ALL` 的次序）。
#[derive(Clone, Debug, Default)]
pub struct Snapshot {
    pub stage_ns: Vec<u64>,
    pub counts: Vec<u64>,
}

#[cfg(feature = "aux-prof")]
mod imp {
    use super::{Counter, Snapshot, Stage};
    use std::cell::RefCell;
    use std::time::Instant;

    thread_local! {
        static ACC: RefCell<([u64; Stage::ALL.len()], [u64; Counter::ALL.len()])> =
            const { RefCell::new(([0; Stage::ALL.len()], [0; Counter::ALL.len()])) };
    }

    pub struct Span(Stage, Instant);

    impl Drop for Span {
        fn drop(&mut self) {
            add_ns(self.0, self.1.elapsed().as_nanos() as u64);
        }
    }

    pub fn span(s: Stage) -> Span {
        Span(s, Instant::now())
    }

    pub fn add_ns(s: Stage, ns: u64) {
        ACC.with(|a| a.borrow_mut().0[s as usize] += ns);
    }

    pub fn count(c: Counter, n: u64) {
        ACC.with(|a| a.borrow_mut().1[c as usize] += n);
    }

    pub fn take() -> Snapshot {
        let (mut s, c) = ACC.with(|a| std::mem::take(&mut *a.borrow_mut()));
        // 细分三项累加的是 TSC 周期，这里换成纳秒；缓存查找那项含 trait 调用，扣掉。
        s[Stage::FineCache as usize] =
            s[Stage::FineCache as usize].saturating_sub(s[Stage::FineTrait as usize]);
        for st in [Stage::FineNth, Stage::FineCache, Stage::FineTrait] {
            s[st as usize] = (s[st as usize] as f64 * super::tsc_ns_per_cycle()) as u64;
        }
        Snapshot {
            stage_ns: s.to_vec(),
            counts: c.to_vec(),
        }
    }
}

#[cfg(not(feature = "aux-prof"))]
mod imp {
    use super::{Counter, Snapshot, Stage};

    pub struct Span;

    // 与开 feature 时同形：调用方 `drop(span)` 收尾一段计时。
    impl Drop for Span {
        #[inline(always)]
        fn drop(&mut self) {}
    }

    #[inline(always)]
    pub fn span(_: Stage) -> Span {
        Span
    }

    #[inline(always)]
    pub fn count(_: Counter, _: u64) {}

    pub fn take() -> Snapshot {
        Snapshot::default()
    }
}

pub use imp::take;
pub(crate) use imp::{count, span};

/// 细分计时：TSC 读数（`aux-prof-fine` 且 x86_64），否则恒 0。
#[inline(always)]
pub(crate) fn fine_now() -> u64 {
    #[cfg(all(feature = "aux-prof-fine", target_arch = "x86_64"))]
    {
        // SAFETY: rdtsc 无内存副作用。
        unsafe { core::arch::x86_64::_rdtsc() }
    }
    #[cfg(not(all(feature = "aux-prof-fine", target_arch = "x86_64")))]
    {
        0
    }
}

/// 把自 `t0`（[`fine_now`]）起的 TSC 周期记到 `s`（[`take`] 时换算成纳秒）。
#[inline(always)]
pub(crate) fn fine_add(_s: Stage, _t0: u64) {
    #[cfg(all(feature = "aux-prof-fine", target_arch = "x86_64"))]
    imp::add_ns(_s, fine_now() - _t0);
}

/// TSC 每周期的纳秒数（首次调用时对着 `Instant` 标定约 20 ms）；未开细分时为 0。
#[cfg(feature = "aux-prof")]
fn tsc_ns_per_cycle() -> f64 {
    #[cfg(all(feature = "aux-prof-fine", target_arch = "x86_64"))]
    {
        static K: std::sync::OnceLock<f64> = std::sync::OnceLock::new();
        *K.get_or_init(|| {
            let t = std::time::Instant::now();
            let c0 = fine_now();
            while t.elapsed() < std::time::Duration::from_millis(20) {}
            let c1 = fine_now();
            t.elapsed().as_nanos() as f64 / (c1 - c0) as f64
        })
    }
    #[cfg(not(all(feature = "aux-prof-fine", target_arch = "x86_64")))]
    {
        0.0
    }
}
