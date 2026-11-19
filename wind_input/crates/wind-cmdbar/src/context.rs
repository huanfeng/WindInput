//! 求值上下文
//!
//! 对照 Go `wind_input/internal/cmdbar/context.go`。`EvalContext` 把宿主运行时状态
//! （编码、上屏历史、剪贴板、前台应用、时间、服务束）暴露给求值器函数。
//! [`MemoryContext`] 是测试友好的内存实现；宿主侧另有适配器。

use crate::services::Services;
use chrono::{DateTime, Local};
use std::sync::Mutex;

/// 求值上下文接口。各取值方法对越界/缺失返回空串（与 Go 同）。
pub trait EvalContext {
    /// 当前输入编码（composition 快照）。
    fn input(&self) -> String;
    /// 倒数第 n 次上屏文本（1-based，1 为最近）；越界返回空。
    fn last(&self, n: i64) -> String;
    /// 剪贴板：n==0 或 1 为当前，n>1 为历史第 n 条（1-based）。
    fn clip(&self, n: i64) -> String;
    /// 前台应用中选中的文本；无选区返回空。
    fn sel(&self) -> String;
    /// 前台进程名（basename）。
    fn app(&self) -> String;
    /// 前台窗口标题。
    fn title(&self) -> String;
    /// 环境变量值；不存在返回空。
    fn env(&self, name: &str) -> String;
    /// 反查一段文本的编码/读音，按 `format` 渲染后返回。查不到任何内容返回空串。
    ///
    /// `format` 是候选注释段的同款 `${name}` 模板（`${char}` / `${code}` / `${pinyin}` /
    /// `${chaizi}` / `${chaizi_code}` / `${dict}`）。**渲染在宿主侧完成**：模板渲染器与
    /// 反查表都住在宿主，而本 crate 刻意零 `wind-*` 依赖；在这里另写一份占位符替换
    /// 就会变成同一套模板语法的第二份实现，两份必然漂移。
    ///
    /// **不给默认实现**，与 [`Self::clip`] 同处置（而不同于 [`Self::quick_var`]）：
    /// 判据是「漏接会怎样」。`quick_var` 漏接得到空串是**正确答案**——短语上下文本就
    /// 没有「当前解析出的年月日」。而本方法漏接得到的空串是**错的**：调用方明明要查，
    /// 拿到空串却只表现为「候选出来了但内容是空的」，没有任何报错。给了默认实现，
    /// 这种漏接就再也没有编译期的抓手。
    fn reverse_lookup(&self, text: &str, format: &str, min_len: usize) -> String;
    /// 当前时间（测试可注入固定时钟）。
    fn now(&self) -> DateTime<Local>;
    /// 副作用服务束；纯求值场景可为 None，动作函数须自行防御。
    fn services(&self) -> Option<&Services>;

    /// 快捷输入格式表的取值口：把「本次输入解析出的量」按名交给 `quick.*` 函数族。
    ///
    /// 名字与 `system.quick.toml` 的 `$` 变量同名同义（`Y` / `MM` / `YC` / `AMT` …），
    /// 两条模板路径因此取到的是同一批值。
    ///
    /// **默认返回 `None`**：只有快捷输入的上下文有「当前解析出的年月日/数值」这回事，
    /// 短语与命令栏的上下文没有。故 `{year()}` 写进 `system.phrases.toml` 只会得到空串，
    /// 而不是拿当前编码硬解出一个假年份——错值比空值难查得多。
    fn quick_var(&self, _name: &str) -> Option<String> {
        None
    }
}

/// 固定容量的上屏历史环形缓冲。index 1 为最近一次 push。
pub struct History {
    inner: Mutex<HistoryInner>,
}

struct HistoryInner {
    buf: Vec<String>,
    cap: usize,
    head: usize, // 下一写位置
    full: bool,
}

impl History {
    /// 构造容量为 `capacity`（下钳到 1）的历史。
    pub fn new(capacity: usize) -> Self {
        let cap = capacity.max(1);
        History {
            inner: Mutex::new(HistoryInner {
                buf: vec![String::new(); cap],
                cap,
                head: 0,
                full: false,
            }),
        }
    }

    /// 记录最近一条上屏。
    pub fn push(&self, s: impl Into<String>) {
        let mut h = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        let head = h.head;
        h.buf[head] = s.into();
        h.head = (head + 1) % h.cap;
        if h.head == 0 {
            h.full = true;
        }
    }

    /// 取倒数第 n 条（1-based）；越界返回空。
    pub fn get(&self, n: i64) -> String {
        let h = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        if n < 1 {
            return String::new();
        }
        let n = n as usize;
        let size = if h.full { h.cap } else { h.head };
        if n > size {
            return String::new();
        }
        let idx = (h.head + h.cap - n) % h.cap;
        h.buf[idx].clone()
    }

    /// 当前条目数。
    pub fn len(&self) -> usize {
        let h = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        if h.full { h.cap } else { h.head }
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// 内存实现的 [`EvalContext`]，主要供测试与纯求值（无宿主）使用。
#[derive(Default)]
pub struct MemoryContext {
    pub input: String,
    pub history: Option<History>,
    /// 当前剪贴板（非空时优先）。
    pub clip: String,
    /// 剪贴板历史，index 0 为最新。
    pub clip_stack: Vec<String>,
    pub sel: String,
    pub app: String,
    pub title: String,
    pub env: std::collections::HashMap<String, String>,
    /// 固定时钟（None 时用 `Local::now()`）。
    pub clock: Option<DateTime<Local>>,
    pub services: Option<Services>,
    /// 反查桩：`(待查文本, format) -> 渲染结果`；`None` 时 `reverse_lookup` 返回空串。
    ///
    /// 用闭包而不是 `HashMap<文本, 结果>`：真实实现里 `format` 决定输出长什么样，
    /// 而映射表把它整个吃掉了——「format 有没有被正确透传下来」就测不出来，
    /// 那恰恰是本层唯一负责的事（本层不渲染，只挑字 + 填默认模板 + 转交）。
    #[allow(clippy::type_complexity)]
    pub reverse: Option<Box<dyn Fn(&str, &str, usize) -> String + Send + Sync>>,
}

impl MemoryContext {
    /// 带 16 条历史缓冲的空上下文。
    pub fn new() -> Self {
        MemoryContext {
            history: Some(History::new(16)),
            ..Default::default()
        }
    }

    pub fn with_input(mut self, input: impl Into<String>) -> Self {
        self.input = input.into();
        self
    }

    pub fn with_services(mut self, s: Services) -> Self {
        self.services = Some(s);
        self
    }
}

impl EvalContext for MemoryContext {
    fn input(&self) -> String {
        self.input.clone()
    }
    fn last(&self, n: i64) -> String {
        match &self.history {
            Some(h) => h.get(n),
            None => String::new(),
        }
    }
    fn clip(&self, n: i64) -> String {
        if n <= 1 {
            if !self.clip.is_empty() || self.clip_stack.is_empty() {
                return self.clip.clone();
            }
            return self.clip_stack[0].clone();
        }
        let idx = (n - 1) as usize;
        self.clip_stack.get(idx).cloned().unwrap_or_default()
    }
    fn sel(&self) -> String {
        self.sel.clone()
    }
    fn app(&self) -> String {
        self.app.clone()
    }
    fn title(&self) -> String {
        self.title.clone()
    }
    fn env(&self, name: &str) -> String {
        self.env.get(name).cloned().unwrap_or_default()
    }
    fn reverse_lookup(&self, text: &str, format: &str, min_len: usize) -> String {
        match &self.reverse {
            Some(f) => f(text, format, min_len),
            None => String::new(),
        }
    }
    fn now(&self) -> DateTime<Local> {
        self.clock.unwrap_or_else(Local::now)
    }
    fn services(&self) -> Option<&Services> {
        self.services.as_ref()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn history_ring() {
        let h = History::new(3);
        assert_eq!(h.get(1), "");
        h.push("a");
        h.push("b");
        h.push("c");
        assert_eq!(h.get(1), "c");
        assert_eq!(h.get(3), "a");
        assert_eq!(h.get(4), "");
        h.push("d"); // 覆盖 a
        assert_eq!(h.get(1), "d");
        assert_eq!(h.get(3), "b");
    }

    #[test]
    fn clip_current_and_history() {
        let mut ctx = MemoryContext::new();
        ctx.clip = "now".into();
        ctx.clip_stack = vec!["h1".into(), "h2".into()];
        assert_eq!(ctx.clip(0), "now");
        assert_eq!(ctx.clip(1), "now");
        assert_eq!(ctx.clip(2), "h2");
        assert_eq!(ctx.clip(9), "");
    }
}
