//! `system.softkeyboard.toml` 的解析、画布展开与三层合并。

use std::collections::BTreeMap;
use std::path::Path;

use serde::Deserialize;
use tracing::warn;

use crate::layout::{KEY_ROWS, normalize_slot, parse_patch_key};

/// 空键位占位符。画布里写它表示「这个键没有映射」——按下吃掉并忽略。
pub const HOLE: &str = ".";

/// 两层：`[0]` 基础层，`[1]` 按住 Shift 的第二层。
type Layers = [Option<String>; 2];

// ───────────────────────── 文件形态（serde） ─────────────────────────

#[derive(Debug, Deserialize)]
struct RawFile {
    #[serde(default)]
    pages: Vec<RawPage>,
}

#[derive(Debug, Deserialize)]
struct RawPage {
    #[serde(default)]
    id: String,
    #[serde(default)]
    name: String,
    /// 基础层画布。**`None` 与 `Some(vec![])` 语义不同**——前者是「用户没写这一项」
    /// （只打补丁），后者是「显式给了一张空画布」（整面替换成空）。用户覆盖的三态
    /// 判据就架在这个区别上，见 [`SoftKeyboardTable::merge_user`]。
    rows: Option<Vec<String>>,
    /// 第二层画布。省略即第二层整层为空。
    rows_shift: Option<Vec<String>>,
    /// 单键补丁，压在画布之上。键名形如 `q` / `shift+q`。
    #[serde(default)]
    keys: BTreeMap<String, String>,
    /// 这一面**发送按键**而不是直接上屏符号。
    ///
    /// `None` = 用户没写这一项（合并时沿用出厂值），与 `Some(false)` 不同。
    send_keys: Option<bool>,
}

// ───────────────────────── 运行时形态 ─────────────────────────

/// 一个面。画布与补丁已在加载期展开合并，运行时只做点查。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Page {
    /// 稳定标识。用户覆盖按它匹配，直通车 action `softkeyboard:<id>` 也认它。
    pub id: String,
    /// 显示名（标签行与面名键上的字）。
    pub name: String,
    /// 这一面**发送按键**，而不是把符号直接塞进宿主。
    ///
    /// # 为什么需要两种面
    ///
    /// 符号面（标点、希腊字母、数学符号）画的是键盘上打不出的字符，点一下就该出那个
    /// 字符——走上屏出口，与自定义标点同族。
    ///
    /// 而**标准 PC 键盘面**画的是键盘本来就有的键。用户点 `n` `i` `h` `a` `o`，期待的是
    /// 打出「你好」，不是往文档里塞五个字母。这一面必须把按键**交还给输入法**：
    /// 鼠标点击合成一次真实按键，物理按键则干脆不接管，两条路都汇进常规输入链路。
    ///
    /// ★ 这不是「哪个面特殊」的硬编码，是面的一个属性：用户自制的键盘面同样可以打开它。
    pub send_keys: bool,
    /// 键位名 → 两层输出。键恒是 [`crate::layout`] 的规范名。
    slots: BTreeMap<&'static str, Layers>,
}

impl Page {
    /// 查一个键位在指定层的输出。`None` = 空键位（吃掉并忽略）。
    ///
    /// `slot` 接受别名与大小写（走 [`normalize_slot`]），调用方不必先规范化。
    pub fn output(&self, slot: &str, shift: bool) -> Option<&str> {
        let canon = normalize_slot(slot)?;
        self.slots
            .get(canon)
            .and_then(|l| l[usize::from(shift)].as_deref())
    }

    /// 该面是否有任何映射。整面为空的面不该出现在标签行里。
    pub fn is_empty(&self) -> bool {
        self.slots
            .values()
            .all(|l| l[0].is_none() && l[1].is_none())
    }

    /// 遍历全部有映射的键位：`(键位名, 层, 输出)`。UI 绘制与测试用。
    pub fn entries(&self) -> impl Iterator<Item = (&'static str, bool, &str)> + '_ {
        self.slots.iter().flat_map(|(k, l)| {
            [(false, l[0].as_deref()), (true, l[1].as_deref())]
                .into_iter()
                .filter_map(move |(shift, v)| v.map(|v| (*k, shift, v)))
        })
    }

    /// 从文件形态展开。整面语义：画布铺底 → 补丁覆盖。
    fn from_raw(raw: &RawPage) -> Self {
        let mut slots: BTreeMap<&'static str, Layers> = BTreeMap::new();
        if let Some(rows) = &raw.rows {
            paint(&mut slots, rows, 0, &raw.id);
        }
        if let Some(rows) = &raw.rows_shift {
            paint(&mut slots, rows, 1, &raw.id);
        }
        let mut page = Self {
            id: raw.id.clone(),
            name: raw.name.clone(),
            send_keys: raw.send_keys.unwrap_or(false),
            slots,
        };
        page.apply_patch(&raw.keys);
        page
    }

    /// 应用单键补丁。认不出的键名**跳过并告警**——静默丢弃会让用户看到「配了没反应」
    /// 却无从分辨是拼错还是功能没做。
    fn apply_patch(&mut self, keys: &BTreeMap<String, String>) {
        for (name, value) in keys {
            let Some((slot, shift)) = parse_patch_key(name) else {
                warn!(
                    "软键盘: 面 {} 跳过补丁键 {:?} —— 不是可映射键位（功能键为封闭集，且只认 shift+ 前缀）",
                    self.id, name
                );
                continue;
            };
            let entry = self.slots.entry(slot).or_default();
            entry[usize::from(shift)] = decode_token(value);
        }
    }
}

/// 把一行行画布刷进 `slots` 的指定层。
///
/// 行数与 token 数都按 [`KEY_ROWS`] 截断并告警：多出来的部分没有键位可落，
/// 静默丢弃的话作者会以为自己写的符号进去了。
fn paint(slots: &mut BTreeMap<&'static str, Layers>, rows: &[String], layer: usize, page_id: &str) {
    if rows.len() > KEY_ROWS.len() {
        warn!(
            "软键盘: 面 {} 的画布有 {} 行，只用前 {} 行（数字行 / QWERTY / ASDF / ZXCV）",
            page_id,
            rows.len(),
            KEY_ROWS.len()
        );
    }
    for (r, line) in rows.iter().take(KEY_ROWS.len()).enumerate() {
        let tokens = split_tokens(line);
        let names = KEY_ROWS[r];
        if tokens.len() > names.len() {
            warn!(
                "软键盘: 面 {} 第 {} 行有 {} 个 token，该行只有 {} 个键位，多出的已忽略",
                page_id,
                r + 1,
                tokens.len(),
                names.len()
            );
        }
        for (c, tok) in tokens.iter().take(names.len()).enumerate() {
            let entry = slots.entry(names[c]).or_default();
            entry[layer] = decode_token(tok);
        }
    }
}

/// 按 **ASCII 空格 / 制表符**切分，不用 `split_whitespace`。
///
/// ★ `char::is_whitespace` 认 U+3000 表意空格，而那正是一个要能写进画布的**符号**
/// （全角空格）。用 `split_whitespace` 会把它当分隔符吃掉，且没有任何报错。
fn split_tokens(line: &str) -> Vec<&str> {
    line.split([' ', '\t']).filter(|s| !s.is_empty()).collect()
}

/// token → 输出。`.` 是空键位占位；`\.` 是字面点；其余原样。
fn decode_token(tok: &str) -> Option<String> {
    match tok {
        HOLE => None,
        "\\." => Some(HOLE.to_string()),
        _ => Some(tok.to_string()),
    }
}

// ───────────────────────── 表 ─────────────────────────

/// 软键盘映射表。面按文件出现序排列，顺序即切面顺序。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SoftKeyboardTable {
    pages: Vec<Page>,
}

impl SoftKeyboardTable {
    /// 面的只读视图。
    pub fn pages(&self) -> &[Page] {
        &self.pages
    }

    pub fn len(&self) -> usize {
        self.pages.len()
    }

    pub fn is_empty(&self) -> bool {
        self.pages.is_empty()
    }

    /// 按 id 取面。
    pub fn page(&self, id: &str) -> Option<&Page> {
        self.pages.iter().find(|p| p.id == id)
    }

    /// 按 id 取面的下标（切面用）。
    pub fn index_of(&self, id: &str) -> Option<usize> {
        self.pages.iter().position(|p| p.id == id)
    }

    /// 解析 TOML 文本。整份语法错误返回 `Err`（调用方回落出厂表）；
    /// **单个面非法只剔除该面**并告警——一个面写错不该让整个软键盘不可用。
    pub fn parse(toml_text: &str) -> Result<Self, toml::de::Error> {
        let raw: RawFile = toml::from_str(toml_text)?;
        let mut pages: Vec<Page> = Vec::with_capacity(raw.pages.len());
        for rp in &raw.pages {
            if rp.id.trim().is_empty() {
                warn!("软键盘: 跳过一个没有 id 的面（id 是用户覆盖与直通车的匹配依据）");
                continue;
            }
            if pages.iter().any(|p| p.id == rp.id) {
                warn!("软键盘: 跳过重复 id={}", rp.id);
                continue;
            }
            let page = Page::from_raw(rp);
            if page.is_empty() {
                warn!("软键盘: 跳过面 {} —— 一个键位都没有映射", rp.id);
                continue;
            }
            pages.push(page);
        }
        Ok(Self { pages })
    }

    /// 从解析好的路径加载；`None`、读取失败或语法错误一律回落 [`Self::builtin`] 并告警。
    ///
    /// 配置坏掉不能导致「功能整个消失」。路径解析是调用方的事
    /// （`Config::resolve_data_file`，含用户覆盖日志）。
    pub fn load(path: Option<&Path>) -> Self {
        let Some(path) = path else {
            warn!("软键盘: system.softkeyboard.toml 两处均不存在，回落内置兜底表（部署可能损坏）");
            return Self::builtin();
        };
        let text = match std::fs::read_to_string(path) {
            Ok(t) => t,
            Err(e) => {
                warn!("软键盘: 读取失败，回落内置兜底表 {}: {}", path.display(), e);
                return Self::builtin();
            }
        };
        match Self::parse(&text) {
            Ok(t) if !t.is_empty() => t,
            Ok(_) => {
                warn!(
                    "软键盘: {} 解析后一个面都不剩，回落内置兜底表",
                    path.display()
                );
                Self::builtin()
            }
            Err(e) => {
                warn!("软键盘: 解析失败，回落内置兜底表 {}: {}", path.display(), e);
                Self::builtin()
            }
        }
    }

    /// 合并用户覆盖文件。**三态语义**（见 `docs/design/soft-keyboard.md` §2.3）：
    ///
    /// | 用户写了什么 | 语义 |
    /// |---|---|
    /// | `rows` 缺失 | 不动基础画布，只应用 `keys` 补丁 |
    /// | `rows` 存在 | 整面画布替换（`keys` 仍在其上叠加） |
    /// | `id` 不匹配任何出厂面 | 新增一个面，追加在末尾 |
    ///
    /// ⚠️ 本函数**只读用户文件、只写内存表**。用户文件本身永远由用户手写，
    /// 程序不回写——写侧纪律见设计文档 §2.2，破坏它就是「整体替换 ⊕ 稀疏 diff」的数据丢失。
    pub fn merge_user(&mut self, toml_text: &str) -> Result<(), toml::de::Error> {
        let raw: RawFile = toml::from_str(toml_text)?;
        for rp in &raw.pages {
            if rp.id.trim().is_empty() {
                warn!("软键盘用户覆盖: 跳过一个没有 id 的面");
                continue;
            }
            match self.pages.iter_mut().find(|p| p.id == rp.id) {
                Some(existing) if rp.rows.is_some() => {
                    // 整面替换：名字与 send_keys 缺省时保留出厂值——用户多半只想换画布，
                    // 不该顺带把标签清空、把 PC 键盘面降级成符号面。
                    let mut fresh = Page::from_raw(rp);
                    if fresh.name.is_empty() {
                        fresh.name = existing.name.clone();
                    }
                    if rp.send_keys.is_none() {
                        fresh.send_keys = existing.send_keys;
                    }
                    *existing = fresh;
                }
                Some(existing) => {
                    if !rp.name.is_empty() {
                        existing.name = rp.name.clone();
                    }
                    if let Some(v) = rp.send_keys {
                        existing.send_keys = v;
                    }
                    existing.apply_patch(&rp.keys);
                }
                None => {
                    let page = Page::from_raw(rp);
                    if page.is_empty() {
                        warn!("软键盘用户覆盖: 跳过新增面 {} —— 一个键位都没有映射", rp.id);
                        continue;
                    }
                    self.pages.push(page);
                }
            }
        }
        Ok(())
    }

    /// 代码内置的兜底表（出厂文件缺失/损坏时用）。
    ///
    /// 刻意只有一面标点：兜底的职责是「别让功能整个消失」，不是复刻出厂内容。
    /// 复刻一份就等于第二真相源，出厂文件改了这里必然漂移。
    pub fn builtin() -> Self {
        Self::parse(BUILTIN).expect("内置兜底表必须能解析")
    }
}

/// 兜底表：一面中文标点。
const BUILTIN: &str = r#"
[[pages]]
id = "punct"
name = "标点"
rows = [
  "· ！ ？ 。 ， ； … — ‧ （ ） 〜 ＝",
  "“ ” ‘ ’ 《 》 【 】 ｛ ｝ 〔 〕 ＼",
  "、 。 ， ； ： ？ ！ ‥ ※ ： ＇",
  "〃 〆 ‰ § † ‖ 〓 ＜ ＞ ／",
]
rows_shift = [
  "～ ¡ ¿ ． 、 ： ‥ － · 〔 〕 ‐ ≡",
  "「 」 『 』 〈 〉 〖 〗 ｟ ｠ ⁅ ⁆ ﹨",
  "﹑ ｡ ﹐ ﹔ ﹕ ﹖ ﹗ ⋯ ＊ ﹕ ＂",
  "〝 〟 ％ ¶ ‡ ∥ ▬ ≪ ≫ ÷",
]
"#;

#[cfg(test)]
mod tests {
    use super::*;

    fn one_page(body: &str) -> SoftKeyboardTable {
        SoftKeyboardTable::parse(body).expect("解析失败")
    }

    #[test]
    fn canvas_maps_tokens_by_position() {
        let t = one_page(
            r#"
[[pages]]
id = "t"
name = "T"
rows = ["A B C", "D E F"]
"#,
        );
        let p = t.page("t").unwrap();
        assert_eq!(p.output("grave", false), Some("A"));
        assert_eq!(p.output("1", false), Some("B"));
        assert_eq!(p.output("2", false), Some("C"));
        assert_eq!(p.output("q", false), Some("D"));
        assert_eq!(p.output("w", false), Some("E"));
        assert_eq!(p.output("e", false), Some("F"));
        // 没铺到的键位是空的
        assert_eq!(p.output("3", false), None);
        assert_eq!(p.output("z", false), None);
    }

    #[test]
    fn hole_and_escaped_dot() {
        let t = one_page(
            r#"
[[pages]]
id = "t"
rows = ["A . \\. B"]
"#,
        );
        let p = t.page("t").unwrap();
        assert_eq!(p.output("grave", false), Some("A"));
        assert_eq!(p.output("1", false), None, ". 是空键位占位");
        assert_eq!(p.output("2", false), Some("."), "\\. 是字面点");
        assert_eq!(p.output("3", false), Some("B"));
    }

    #[test]
    fn ideographic_space_survives_tokenizing() {
        // U+3000 是 char::is_whitespace，split_whitespace 会把它吃掉。
        let t = one_page(
            r#"
[[pages]]
id = "t"
rows = ["A 　 B"]
"#,
        );
        let p = t.page("t").unwrap();
        assert_eq!(p.output("grave", false), Some("A"));
        assert_eq!(
            p.output("1", false),
            Some("\u{3000}"),
            "全角空格应作为符号保留"
        );
        assert_eq!(p.output("2", false), Some("B"));
    }

    #[test]
    fn shift_layer_is_separate() {
        let t = one_page(
            r#"
[[pages]]
id = "t"
rows = ["a b"]
rows_shift = ["A B"]
"#,
        );
        let p = t.page("t").unwrap();
        assert_eq!(p.output("grave", false), Some("a"));
        assert_eq!(p.output("grave", true), Some("A"));
    }

    #[test]
    fn missing_shift_rows_means_empty_second_layer() {
        let t = one_page(
            r#"
[[pages]]
id = "t"
rows = ["a b"]
"#,
        );
        let p = t.page("t").unwrap();
        assert_eq!(p.output("grave", true), None);
    }

    #[test]
    fn patch_overrides_canvas() {
        let t = one_page(
            r#"
[[pages]]
id = "t"
rows = ["a b"]
keys = { grave = "Z", "shift+1" = "Y" }
"#,
        );
        let p = t.page("t").unwrap();
        assert_eq!(p.output("grave", false), Some("Z"), "补丁压过画布");
        assert_eq!(p.output("1", false), Some("b"), "没打补丁的键位不动");
        assert_eq!(p.output("1", true), Some("Y"), "补丁可以只写第二层");
    }

    #[test]
    fn patch_can_clear_a_slot() {
        let t = one_page(
            r#"
[[pages]]
id = "t"
rows = ["a b"]
keys = { grave = "." }
"#,
        );
        assert_eq!(t.page("t").unwrap().output("grave", false), None);
    }

    #[test]
    fn output_accepts_aliases() {
        let t = one_page(
            r#"
[[pages]]
id = "t"
rows = ["a b c d e f g h i j k l m"]
"#,
        );
        let p = t.page("t").unwrap();
        assert_eq!(p.output("`", false), Some("a"));
        assert_eq!(p.output("GRAVE", false), Some("a"));
        assert_eq!(p.output("-", false), Some("l"));
        assert_eq!(p.output("equals", false), Some("m"));
    }

    #[test]
    fn skips_bad_pages_but_keeps_good_ones() {
        let t = one_page(
            r#"
[[pages]]
id = ""
rows = ["x"]

[[pages]]
id = "good"
rows = ["y"]

[[pages]]
id = "good"
rows = ["z"]

[[pages]]
id = "blank"
rows = [". . ."]
"#,
        );
        assert_eq!(t.len(), 1, "空 id / 重复 id / 全空面都该剔除");
        assert_eq!(t.page("good").unwrap().output("grave", false), Some("y"));
    }

    #[test]
    fn unknown_patch_key_is_skipped_not_fatal() {
        let t = one_page(
            r#"
[[pages]]
id = "t"
rows = ["a"]
keys = { enter = "X", "ctrl+q" = "Y", w = "W" }
"#,
        );
        let p = t.page("t").unwrap();
        assert_eq!(p.output("grave", false), Some("a"));
        assert_eq!(p.output("w", false), Some("W"), "合法补丁照常生效");
    }

    #[test]
    fn extra_rows_and_tokens_are_truncated() {
        let t = one_page(
            r#"
[[pages]]
id = "t"
rows = [
  "1 2 3 4 5 6 7 8 9 10 11 12 13 14 15",
  "a", "b", "c", "d",
]
"#,
        );
        let p = t.page("t").unwrap();
        assert_eq!(
            p.output("equal", false),
            Some("13"),
            "第 13 个 token 落在最后一个键位"
        );
        assert_eq!(p.output("q", false), Some("a"));
        assert_eq!(p.output("z", false), Some("c"), "第 4 行仍然有效");
    }

    // ── 用户覆盖三态 ──

    #[test]
    fn user_patch_only_when_rows_absent() {
        let mut t = one_page(
            r#"
[[pages]]
id = "t"
name = "出厂"
rows = ["a b c"]
"#,
        );
        t.merge_user(
            r#"
[[pages]]
id = "t"
keys = { "1" = "X" }
"#,
        )
        .unwrap();
        let p = t.page("t").unwrap();
        assert_eq!(p.output("grave", false), Some("a"), "基础画布不动");
        assert_eq!(p.output("1", false), Some("X"), "补丁生效");
        assert_eq!(p.output("2", false), Some("c"));
        assert_eq!(p.name, "出厂", "没给 name 就不动");
    }

    #[test]
    fn user_rows_replace_whole_page() {
        let mut t = one_page(
            r#"
[[pages]]
id = "t"
name = "出厂"
rows = ["a b c"]
"#,
        );
        t.merge_user(
            r#"
[[pages]]
id = "t"
rows = ["X"]
"#,
        )
        .unwrap();
        let p = t.page("t").unwrap();
        assert_eq!(p.output("grave", false), Some("X"));
        assert_eq!(p.output("1", false), None, "整面替换，旧画布不残留");
        assert_eq!(p.name, "出厂", "名字缺省时保留出厂名");
    }

    #[test]
    fn user_unknown_id_appends_page() {
        let mut t = one_page(
            r#"
[[pages]]
id = "t"
rows = ["a"]
"#,
        );
        t.merge_user(
            r#"
[[pages]]
id = "mine"
name = "我的"
rows = ["Z"]
"#,
        )
        .unwrap();
        assert_eq!(t.len(), 2);
        assert_eq!(t.index_of("mine"), Some(1), "新增面追加在末尾");
        assert_eq!(t.page("mine").unwrap().name, "我的");
    }

    #[test]
    fn builtin_is_parseable_and_nonempty() {
        let t = SoftKeyboardTable::builtin();
        assert!(!t.is_empty());
        assert_eq!(t.page("punct").unwrap().output("1", false), Some("！"));
    }

    #[test]
    fn entries_lists_both_layers() {
        let t = one_page(
            r#"
[[pages]]
id = "t"
rows = ["a"]
rows_shift = ["A"]
"#,
        );
        let mut got: Vec<_> = t.page("t").unwrap().entries().collect();
        got.sort();
        assert_eq!(got, vec![("grave", false, "a"), ("grave", true, "A")]);
    }
}
