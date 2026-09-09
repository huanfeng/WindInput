//! 标点转换器
//!
//! 与 Go 版本 `wind_input/internal/transform/punctuation.go` 对齐。
//! 英文标点 → 中文标点；引号根据左右状态切换。

use wind_config::config::PunctConfig;

/// 标点转换器（**只**持有引号左右交替状态）。
///
/// 自定义映射表刻意**不**存在这里：它是配置，须每次从实时 `PunctConfig` 读。曾经存过一份
/// 副本，只在 `Coordinator::new` 注入一次 → 设置页保存后不重启服务永不生效（且外层开关读
/// 实时配置、内层数据读旧副本，症状是「开关明明开着却完全无反应」）。转换器只留运行时状态。
pub struct PunctuationConverter {
    single_quote_left: bool,
    double_quote_left: bool,
}

impl Default for PunctuationConverter {
    fn default() -> Self {
        Self::new()
    }
}

impl PunctuationConverter {
    pub fn new() -> Self {
        Self {
            single_quote_left: true,
            double_quote_left: true,
        }
    }

    /// 重置引号状态（模式切换/清空时调用）。
    pub fn reset(&mut self) {
        self.single_quote_left = true;
        self.double_quote_left = true;
    }

    /// 自定义映射的查表键：引号按当前左右态取左形行/右形行，其余键取字符本身。
    pub fn custom_key(&self, c: char) -> String {
        match quote_custom_keys(c) {
            Some((left_key, right_key)) => {
                let is_left = match c {
                    '"' => self.double_quote_left,
                    _ => self.single_quote_left,
                };
                if is_left { left_key } else { right_key }.to_string()
            }
            None => c.to_string(),
        }
    }

    /// 查自定义映射但**不**推进引号态。`col_idx`: 0=中半 1=英全 2=中全 3=英半。
    /// 开关与映射表同取自实时 `PunctConfig`——不得再在别处存副本（见结构体文档）。
    pub fn peek_custom(&self, punct: &PunctConfig, c: char, col_idx: usize) -> Option<String> {
        if !punct.custom_enabled {
            return None;
        }
        let v = punct
            .custom_mappings
            .get(&self.custom_key(c))?
            .get(col_idx)?;
        (!v.is_empty()).then(|| v.clone())
    }

    /// 查自定义映射；命中（非空）时推进引号交替态并返回，未命中不动状态。
    /// 对齐 Go `PunctuationConverter.LookupCustom`。
    pub fn lookup_custom(
        &mut self,
        punct: &PunctConfig,
        c: char,
        col_idx: usize,
    ) -> Option<String> {
        let v = self.peek_custom(punct, c, col_idx)?;
        match c {
            '"' => self.double_quote_left = !self.double_quote_left,
            '\'' => self.single_quote_left = !self.single_quote_left,
            _ => {}
        }
        Some(v)
    }

    /// 预测 `c` 的中文标点产物但**不**改引号状态（智能符号武装/匹配用）。
    /// 对齐 Go `PeekChineseStr`。返回 None 表示该键无中文标点映射。
    pub fn peek_chinese_str(&self, c: char) -> Option<String> {
        match c {
            '\'' => Some(
                if self.single_quote_left {
                    '\u{2018}'
                } else {
                    '\u{2019}'
                }
                .to_string(),
            ),
            '"' => Some(
                if self.double_quote_left {
                    '\u{201C}'
                } else {
                    '\u{201D}'
                }
                .to_string(),
            ),
            _ => Self::static_chinese(c),
        }
    }

    /// 回退一次引号交替（智能符号吃掉一个引号后调用，使下次同引号仍从左引号开始）。
    /// 对齐 Go `RevertLastQuote`。
    pub fn revert_last_quote(&mut self, c: char) {
        match c {
            '\'' => self.single_quote_left = !self.single_quote_left,
            '"' => self.double_quote_left = !self.double_quote_left,
            _ => {}
        }
    }

    /// 把某引号的交替态强制置为「左」。**自动配对生效时使用**：右引号由配对补出，
    /// 一次按键即产出完整一对，交替开关不该随之前进；钉死在左才能保证每次按键都开新的一对。
    /// 不钉则交替开关与配对栈错位 → 「一次出对、一次出单」循环（见 [`quote_pair`]）。
    pub fn pin_quote_left(&mut self, c: char) {
        match c {
            '\'' => self.single_quote_left = true,
            '"' => self.double_quote_left = true,
            _ => {}
        }
    }

    /// 无状态中文标点映射（非引号部分），供 to_chinese / peek 共用。
    fn static_chinese(c: char) -> Option<String> {
        match c {
            '^' => return Some("\u{2026}\u{2026}".to_string()), // ……
            '_' => return Some("\u{2014}\u{2014}".to_string()), // ——
            _ => {}
        }
        let mapped = match c {
            ',' => '\u{FF0C}',  // ，
            '.' => '\u{3002}',  // 。
            '?' => '\u{FF1F}',  // ？
            '!' => '\u{FF01}',  // ！
            ':' => '\u{FF1A}',  // ：
            ';' => '\u{FF1B}',  // ；
            '(' => '\u{FF08}',  // （
            ')' => '\u{FF09}',  // ）
            '[' => '\u{3010}',  // 【
            ']' => '\u{3011}',  // 】
            '{' => '\u{FF5B}',  // ｛
            '}' => '\u{FF5D}',  // ｝
            '<' => '\u{300A}',  // 《
            '>' => '\u{300B}',  // 》
            '~' => '\u{FF5E}',  // ～
            '$' => '\u{FFE5}',  // ￥
            '`' => '\u{00B7}',  // ·
            '\\' => '\u{3001}', // 、
            _ => return None,
        };
        Some(mapped.to_string())
    }

    /// 英文标点 → 中文标点；返回 None 表示该字符无中文标点映射。
    /// 结果可能是多字符（如 `^`→`……`），故返回 String。
    pub fn to_chinese(&mut self, c: char) -> Option<String> {
        // 引号需切换左右
        match c {
            '\'' => {
                let r = if self.single_quote_left {
                    '\u{2018}'
                } else {
                    '\u{2019}'
                };
                self.single_quote_left = !self.single_quote_left;
                return Some(r.to_string());
            }
            '"' => {
                let r = if self.double_quote_left {
                    '\u{201C}'
                } else {
                    '\u{201D}'
                };
                self.double_quote_left = !self.double_quote_left;
                return Some(r.to_string());
            }
            _ => {}
        }
        Self::static_chinese(c)
    }
}

/// 中文标点 → 产生它的 **ASCII 源键**。[`PunctuationConverter::static_chinese`] 的反向枚举。
///
/// # 谁要它
///
/// 软键盘的键面上直接写着 `，`、`、`、`……`，但引擎那边**不存在「直接产出中文标点的键」**：
/// 中文标点是 ASCII 键经标点层转换来的，转成什么由中英标点态、自定义映射、智能符号共同
/// 决定。宿主若把键面上那个 `，` 原样上屏，这三样对它全部失效——而设置页里的开关照样
/// 摆着，点了没反应也不报错。
///
/// 所以宿主按下键面上的中文标点时，要送的是它的 ASCII 源键。这张表由正向表反向枚举，
/// 不另写一份：宿主自己抄一张，正向表加了新映射它不会跟着改，新键就会绕过标点层。
///
/// 两处细节：
/// - 多字符产物（`^`→`……`、`_`→`——`）按其**组成字符**登记，因为键面上写的是单个 `…`；
/// - 引号有左右状态、不在 `static_chinese` 里，故单列；左右两形回到同一个 ASCII 键。
pub fn chinese_punct_sources() -> Vec<(char, char)> {
    let mut out: Vec<(char, char)> = Vec::new();
    let mut push = |cn: char, ascii: char| {
        if !out.iter().any(|(c, _)| *c == cn) {
            out.push((cn, ascii));
        }
    };
    for ascii in ' '..='~' {
        if let Some(s) = PunctuationConverter::static_chinese(ascii) {
            for cn in s.chars() {
                push(cn, ascii);
            }
        }
    }
    for (cn, ascii) in [
        ('\u{2018}', '\''),
        ('\u{2019}', '\''),
        ('\u{201C}', '"'),
        ('\u{201D}', '"'),
    ] {
        push(cn, ascii);
    }
    out
}

/// 引号键在自定义映射里的两行键名 **(左形行, 右形行)**；非引号键返回 None。
///
/// **存储键格式的唯一定义处**（跨仓的第二个知情者是设置端 `PUNCT_DEFAULTS` 的 token 列）。
///
/// 语义要点：这两行界面上叫「第一次 / 第二次」，实质是**左形 / 右形**——「第几次」只是没有
/// 自动配对时按次序推导左右角色的说法。配对生效后一次按键同时产出左右两个符号，就得同时取用
/// 这两行（见 `wind_punct::quote_forms`），否则交替态被钉左、「第二次」那行永远取不到。
pub fn quote_custom_keys(c: char) -> Option<(&'static str, &'static str)> {
    match c {
        '"' => Some(("\"1", "\"2")),
        '\'' => Some(("'1", "'2")),
        _ => None,
    }
}

/// 自定义映射行键 → 源字符（[`quote_custom_keys`] 的反函数）：`"1`/`"2`→`"`、`'1`/`'2`→`'`、
/// 单字符键即其本身；多字符或未知格式返回 None。
///
/// 用途：从 `custom_mappings` 反推「哪些按键被用户覆盖过」（如告知 TSF 英文模式下该吃哪些
/// 标点键）。与 `quote_custom_keys` 成对维护——键格式一旦变化，两侧必须同时改。
pub fn custom_key_source_char(key: &str) -> Option<char> {
    for c in ['"', '\''] {
        if let Some((left_key, right_key)) = quote_custom_keys(c)
            && (key == left_key || key == right_key)
        {
            return Some(c);
        }
    }
    let mut it = key.chars();
    match (it.next(), it.next()) {
        (Some(c), None) => Some(c),
        _ => None,
    }
}

/// 引号键的「左形 / 右形」**内置**中文产物，与交替状态和自定义映射均无关；非引号键返回 None。
/// 含自定义映射的版本见 `wind_punct::quote_forms`。
///
/// 引号是唯一的**对称配对键**：同一个物理键既可能产出左引号也可能产出右引号，按键本身
/// 不携带「这是开还是闭」这一位信息。其它配对符（`（` 与 `）`）是两个不同的键，天然携带。
/// 故自动配对生效时，引号只能一律按「开一对」处理，跳出交给 `auto_pair.jump_out_keys`。
pub fn quote_pair(c: char) -> Option<(char, char)> {
    match c {
        '\'' => Some(('\u{2018}', '\u{2019}')),
        '"' => Some(('\u{201C}', '\u{201D}')),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_basic_punct() {
        let mut p = PunctuationConverter::new();
        assert_eq!(p.to_chinese(',').as_deref(), Some("，"));
        assert_eq!(p.to_chinese('.').as_deref(), Some("。"));
        assert_eq!(p.to_chinese('\\').as_deref(), Some("、"));
        assert_eq!(p.to_chinese('^').as_deref(), Some("……"));
        assert_eq!(p.to_chinese('a'), None);
    }

    /// 测试用自定义映射配置：'/' 四列全配 + 双引号左右分键（仅中文半角列）。
    fn custom_cfg(enabled: bool) -> PunctConfig {
        let mut c = PunctConfig {
            custom_enabled: enabled,
            ..PunctConfig::default()
        };
        // '/' 自定义：中半=、 英全=／ 中全=、 英半=/
        c.custom_mappings.insert(
            "/".to_string(),
            vec!["、".into(), "／".into(), "、".into(), "/".into()],
        );
        c.custom_mappings
            .insert("\"1".to_string(), vec!["「".into()]);
        c.custom_mappings
            .insert("\"2".to_string(), vec!["」".into()]);
        c
    }

    #[test]
    fn test_custom_mapping() {
        let mut p = PunctuationConverter::new();
        let cfg = custom_cfg(true);
        assert_eq!(p.lookup_custom(&cfg, '/', 0).as_deref(), Some("、")); // 中文半角
        assert_eq!(p.lookup_custom(&cfg, '/', 1).as_deref(), Some("／")); // 英文全角
        assert_eq!(p.lookup_custom(&cfg, '/', 3).as_deref(), Some("/")); // 英文半角
        assert_eq!(p.lookup_custom(&cfg, 'a', 0), None); // 无映射
        // 引号按左右交替选 key 并切换状态
        assert_eq!(p.lookup_custom(&cfg, '"', 0).as_deref(), Some("「")); // 左
        assert_eq!(p.lookup_custom(&cfg, '"', 0).as_deref(), Some("」")); // 右
    }

    #[test]
    fn test_custom_disabled() {
        let mut p = PunctuationConverter::new();
        assert_eq!(p.lookup_custom(&custom_cfg(false), '/', 0), None); // 未启用
    }

    /// 配置是**参数**而非转换器内的副本：同一实例，配置换了下一次查表即跟随
    /// （回归锁：曾把映射表存进转换器且只在启动时注入一次，热重载后仍用旧表）。
    #[test]
    fn custom_mapping_follows_live_config() {
        let mut p = PunctuationConverter::new();
        assert_eq!(p.lookup_custom(&PunctConfig::default(), '/', 0), None); // 出厂无映射
        assert_eq!(
            p.lookup_custom(&custom_cfg(true), '/', 0).as_deref(),
            Some("、")
        );
    }

    /// peek 不推进引号态，且引号也走 `"1`/`"2` 键（曾按 `"` 拼键 → 引号永远查不到自定义）。
    #[test]
    fn peek_custom_quote_key_without_advancing() {
        let p = PunctuationConverter::new();
        let cfg = custom_cfg(true);
        assert_eq!(p.custom_key('"'), "\"1");
        assert_eq!(p.peek_custom(&cfg, '"', 0).as_deref(), Some("「"));
        assert_eq!(p.peek_custom(&cfg, '"', 0).as_deref(), Some("「")); // 未推进
    }

    #[test]
    fn test_peek_and_revert() {
        let mut p = PunctuationConverter::new();
        // peek 不改状态：连续 peek 同引号返回相同（左）
        assert_eq!(p.peek_chinese_str('"').as_deref(), Some("\u{201C}"));
        assert_eq!(p.peek_chinese_str('"').as_deref(), Some("\u{201C}"));
        assert_eq!(p.peek_chinese_str('.').as_deref(), Some("。"));
        // to_chinese 推进到右，revert 退回左
        assert_eq!(p.to_chinese('"').as_deref(), Some("\u{201C}")); // 左→推进
        p.revert_last_quote('"');
        assert_eq!(p.to_chinese('"').as_deref(), Some("\u{201C}")); // revert 后仍为左
    }

    #[test]
    fn test_quote_toggle() {
        let mut p = PunctuationConverter::new();
        assert_eq!(p.to_chinese('"').as_deref(), Some("\u{201C}")); // 左
        assert_eq!(p.to_chinese('"').as_deref(), Some("\u{201D}")); // 右
        assert_eq!(p.to_chinese('\'').as_deref(), Some("\u{2018}"));
        p.reset();
        assert_eq!(p.to_chinese('"').as_deref(), Some("\u{201C}")); // reset 后回到左
    }
}
