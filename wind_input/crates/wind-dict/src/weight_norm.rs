//! 词库权重归一化：把偏离约定值域的词库权重映射回 `0 ~ WEIGHT_RANGE_MAX`。
//!
//! 存在的理由见 [`crate::WEIGHT_RANGE_MAX`] 与 `docs/design/dict-weight-normalization.md`：
//! 短语权重与码表权重**设计上同轴**（都规范在 0~10000），同轴才使「短语 vs 码表」的权重
//! 比较有意义。但这条约定从未被执行，虎码这类词库直接搬了原始语料词频（max 一千万），
//! 于是短语权重拉满也压不过它。
//!
//! ## 为什么是**按库 opt-in** 而不是全局强制
//!
//! 守约的词库并非均匀分布——五笔 p50=941、max=9999，中位落在 9.4% 的位置。若强制把所有库
//! 拉平（如分位映射到 p50→5000），五笔的中位会从 941 跳到 5000，短语默认权重 1000 就从
//! 「略高于中位」变成「远低于中位」。**那是无谓的行为变更**。故只有显式配了
//! `[dictionaries.weight_spec]` 的库才归一化。
//!
//! ## ⚠️ 为什么不能线性压缩
//!
//! 按 max 线性压缩虎码，压缩因子 ≈1035，p50 的 397 会被**整数除法归零**。这与刚拆掉的
//! `PINYIN_TIER_SCALE`(÷100) 把拼音 p50=34 归零是同一个错误：长尾分布做线性压缩，等于把
//! 绝大多数条目压进同一个值，量程全给了极少数头部词。故默认且推荐 `mode = "log"`。

use crate::WEIGHT_RANGE_MAX;

/// 归一化参数（`[dictionaries.weight_spec]` 的**无依赖镜像**）。
///
/// 与 `wind_config::WeightSpec` 同构但不引用它——`wind-dict` 不依赖 `wind-config`，
/// 由调用方（`wind-engine::manager`）转换后传入。同 `ShadowPinRule` 之于 `wind-store`。
#[derive(Debug, Clone, Copy)]
pub struct WeightNorm {
    /// 本库权重的中位数（**方案声明值**，不是运行时实测——理由见设计文档 §4.3）。
    median: f64,
    /// 本库权重的最大值（方案声明值）。
    max: f64,
    /// `median` 归一化后的落点。默认 [`DEFAULT_TARGET`]。
    target: f64,
    /// true = 对数映射（推荐），false = 线性。
    log_mode: bool,
}

/// `median` 的默认落点：与短语默认权重（1000，"中位"）及五笔四码全码 p50(895) 同量级。
pub const DEFAULT_TARGET: i64 = 1_000;

impl WeightNorm {
    /// 从声明值构造。参数不自洽时返回 `None`（调用方应告警并跳过归一化，**不要静默按恒等处理**
    /// ——那会让配错的方案看起来"配了但没用"）。
    ///
    /// 自洽要求：`0 < median < max` 且 `0 < target < WEIGHT_RANGE_MAX`。
    pub fn from_parts(median: i64, max: i64, mode: &str, target: i64) -> Option<Self> {
        let target = if target > 0 { target } else { DEFAULT_TARGET };
        if median <= 0 || max <= median || target <= 0 || target >= WEIGHT_RANGE_MAX as i64 {
            return None;
        }
        Some(Self {
            median: median as f64,
            max: max as f64,
            target: target as f64,
            // 未声明 mode 时取对数：线性对长尾分布是错的（见模块文档）。
            log_mode: !mode.eq_ignore_ascii_case("linear"),
        })
    }

    /// 映射单条权重。**保序**，且非零权重恒映射到 ≥1（不归零）。
    ///
    /// 分两段线性插值（log 模式下在对数空间），锚点为 `median → target`、`max → 上界`：
    /// 这样「本库的中位」与「短语的中位」对齐，两条轴才真的可比。
    pub fn apply(&self, w: i32) -> i32 {
        // 权重 0 = 无权重列/空列，是「未定义」不是「最低」。归一化不该凭空给它造一个值——
        // 需要让这类库参与权重比较的，用 `[[dictionaries]].default_weight` 显式定档。
        if w <= 0 {
            return w;
        }
        let w = w as f64;
        let f = |x: f64| if self.log_mode { (1.0 + x).ln() } else { x };
        let (fw, fmed, fmax) = (f(w), f(self.median), f(self.max));
        let out = if fw <= fmed {
            // 低半段：[0, median] → [0, target]
            self.target * fw / fmed
        } else {
            // 高半段：[median, max] → [target, 上界]。超过声明 max 的条目会被 clamp 压住——
            // 那说明声明值本身过时了，诊断会另行报出来。
            let hi = WEIGHT_RANGE_MAX as f64;
            self.target + (hi - self.target) * (fw - fmed) / (fmax - fmed).max(f64::EPSILON)
        };
        (out.round() as i32).clamp(1, WEIGHT_RANGE_MAX)
    }
}

#[cfg(test)]
#[path = "weight_norm_tests.rs"]
mod tests;
