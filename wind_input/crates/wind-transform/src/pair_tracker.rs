//! 自动配对标点跟踪器
//!
//! 与 Go 版本 `wind_input/internal/transform/pair_tracker.go` 对齐。
//!
//! 本栈是配对状态的**唯一真相源**（中文、英文全角、英文自定义标点、英文半角四条建立路径
//! 全部入此栈）。DLL 侧的 `_pairPendingDepth` 只是它的镜像计数，充当吃键闸门。
//!
//! 除了栈本身，还携带两项元数据，用来回答「这份状态还作不作数」：
//!
//! - `owner_token`：本栈是全局单栈、不分宿主。用户在 A 的括号里被弹框打断、切到 B 打字后，
//!   栈顶可能已是 B 压的那层，故聚焦时须校验归属。
//! - `last_activity`：失焦不再一律清栈之后，需要时效兜底——用户中途用鼠标点走、删掉括号
//!   这类操作输入法感知不到，没有时效的话陈旧状态会一直存活到吃掉用户的 Tab。

use std::time::Instant;

/// 配对条目
///
/// 左右两段都是**字符串**而非单字符：标点配对键恒为单字符，但直通命令
/// （`ime.pair`）可以压入任意模板（`<!--` / `-->`、`"""` / `"""`），存不下就只能
/// 退回「上屏对了、跳不出去」的老路。
#[derive(Debug, Clone)]
pub struct PairEntry {
    pub left: String,
    pub right: String,
    /// 跳出本层时光标要右移的格数（合成几次 VK_RIGHT）。
    ///
    /// **不等于 `right` 的长度**：VK_RIGHT 一次越过多少内容是宿主行为——emoji 与组合字
    /// 在多数宿主一次跨整个字素簇、少数宿主按 UTF-16 单元走。故此处存的是调用方算好或
    /// 显式指定的格数，跳出路径照搬，不再二次推导。
    pub jump_steps: u32,
}

impl PairEntry {
    /// 右段是否恰为单个字符 `c`。
    ///
    /// 右符号跳出（`jump_out_on_right_symbol`）只可能由单个标点按键触发，故只有单字符
    /// 右段配得上；直通压入的多字符右段在此恒为 false，只能靠 Tab/Enter 跳出。
    pub fn right_is_char(&self, c: char) -> bool {
        let mut it = self.right.chars();
        it.next() == Some(c) && it.next().is_none()
    }
}

/// 配对跟踪器
pub struct PairTracker {
    stack: Vec<PairEntry>,
    /// 压入首层配对时的客户端 token（0 = 未归属）。
    owner_token: u64,
    /// 最后一次活动时间。`None` = 从未活动（此时栈必为空）。
    last_activity: Option<Instant>,
}

impl PairTracker {
    pub fn new() -> Self {
        Self {
            stack: Vec::new(),
            owner_token: 0,
            last_activity: None,
        }
    }

    /// 压入单字符配对（标点配对路径）。跳出恒右移 1 格。
    pub fn push(&mut self, left: char, right: char) {
        self.push_at(left, right, Instant::now());
    }

    /// [`Self::push`] 的可注入时钟版本（测试用）。
    pub fn push_at(&mut self, left: char, right: char, now: Instant) {
        self.push_text_at(left.to_string(), right.to_string(), 1, now);
    }

    /// 压入任意文本配对（直通 `ime.pair` 路径），跳出格数由调用方指定。
    pub fn push_text(&mut self, left: String, right: String, jump_steps: u32) {
        self.push_text_at(left, right, jump_steps, Instant::now());
    }

    /// [`Self::push_text`] 的可注入时钟版本（测试用）。
    pub fn push_text_at(&mut self, left: String, right: String, jump_steps: u32, now: Instant) {
        self.stack.push(PairEntry {
            left,
            right,
            jump_steps,
        });
        self.last_activity = Some(now);
    }

    /// 查看栈顶
    pub fn peek(&self) -> Option<&PairEntry> {
        self.stack.last()
    }

    /// 弹出栈顶
    pub fn pop(&mut self) -> Option<PairEntry> {
        self.stack.pop()
    }

    pub fn is_empty(&self) -> bool {
        self.stack.is_empty()
    }

    /// 清空（连同归属与活动时间一起复位，避免下次压栈继承旧归属）
    pub fn clear(&mut self) {
        self.stack.clear();
        self.owner_token = 0;
        self.last_activity = None;
    }

    /// 归属的客户端 token（0 = 未归属）
    pub fn owner_token(&self) -> u64 {
        self.owner_token
    }

    /// 认领归属。仅在栈由空变为非空时有意义，故由调用方在压首层前设置。
    pub fn set_owner_token(&mut self, token: u64) {
        self.owner_token = token;
    }

    /// 刷新活动时间（每次按键都应调用）。栈空时不记——没有状态需要保活。
    pub fn touch(&mut self) {
        self.touch_at(Instant::now());
    }

    /// [`Self::touch`] 的可注入时钟版本（测试用）。
    pub fn touch_at(&mut self, now: Instant) {
        if !self.stack.is_empty() {
            self.last_activity = Some(now);
        }
    }

    /// 状态是否已陈旧。`ttl_secs == 0` = 永不过期。
    ///
    /// 栈空或从未活动过时恒返回 `false`——那种情况下没有状态可谈，
    /// 判定交给调用方的「栈非空」前置条件。
    pub fn is_stale(&self, ttl_secs: u32) -> bool {
        self.is_stale_at(Instant::now(), ttl_secs)
    }

    /// [`Self::is_stale`] 的可注入时钟版本（测试用）。
    pub fn is_stale_at(&self, now: Instant, ttl_secs: u32) -> bool {
        if ttl_secs == 0 || self.stack.is_empty() {
            return false;
        }
        match self.last_activity {
            Some(t) => now.saturating_duration_since(t).as_secs() >= ttl_secs as u64,
            None => false,
        }
    }
}

impl Default for PairTracker {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
#[path = "pair_tracker_tests.rs"]
mod tests;
