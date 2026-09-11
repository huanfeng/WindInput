//! 按候选窗**排布**分档的配置值：标量＝横竖共用，分档＝各配一份。
//!
//! # 为什么要有这么个东西
//!
//! 横排与竖排下可用空间差一个数量级（竖排每行独占，横排全部候选共享一行），于是一批
//! 配置项的「合理值不是同一个答案」。此前的做法是**拆键**——`comment_max_chars` 拆成
//! `comment_max_chars_horizontal` / `_vertical`，`min_window_width` 同理，共 4 对 8 个键。
//! 拆键每来一项都要：加两个键、改注册表、写一次迁移、加两个设置项，而且键名越来越长
//! （`ui.candidate.page_number_display_horizontal` 43 个字符），手写配置的人尤其吃亏。
//!
//! 本类型把这件事收敛成**一个字段类型**：
//!
//! ```toml
//! pager_bar_display = "hide"              # 横竖共用
//! pager_bar_display = "h:hide v:always"   # 横排隐藏、竖排常显
//! pager_bar_display = "v:always"          # 只说竖排；横排＝默认（此处即「跟随主题」）
//! pager_bar_display = { h = "hide", v = "always" }   # 表写法，等价
//! ```
//!
//! 键名不变、不加长，老配置**零迁移**（标量写法始终合法）。
//!
//! # ★ 与主题 `Ld` 同构，但序列化不同
//!
//! 主题侧早有同一个形状：[`wind_theme::schema::Ld`]（light/dark 原语，标量＝明暗共用、
//! `{light, dark}`＝分设、求值期 `select(is_dark)` 坍缩成单值）。本类型的**概念与 API
//! 刻意照搬它**——`select(vertical)` 与 `select(is_dark)` 同构，解析同样手写
//! `Deserialize`（先取 `toml::Value` 再 match，容错不 panic）。
//!
//! 唯一不照搬的是 wire format，理由是**两者的写回约束不同**：
//!
//! | | 主题 `Ld` | 本类型 |
//! |---|---|---|
//! | 会不会被程序写回 | **不会**（`wind-theme` 全 crate 无序列化） | **会**（`to_string_pretty` 整表写回） |
//! | 默认输出形态 | 不适用 | 标签式 `"h:… v:…"` |
//!
//! ⚠️ TOML 的排版规则要求表排在标量之后，所以**表写法一旦被写回就会裂成子段**
//! （`[ui.candidate.pager_bar_display]`），同段的标量字段被迫上浮、顺序被打乱，一项一个
//! 子段。主题不写回故无此问题；配置写回是常态，故默认输出标签式，形态不漂移。
//!
//! 两种写法**都读得懂**，用户手写哪种都行。
//!
//! # ⚠️ 缺侧语义与 `Ld` 相反，这是刻意的
//!
//! `Ld` 缺一侧回退**另一侧**（颜色至少得有一个值）。本类型缺一侧回退**该类型的默认值**
//! ——因为这些项的语义是「覆盖」，默认值就是「不覆盖」。写 `"v:always"` 读作「竖排常显，
//! 横排没说」，若照搬 `Ld` 回退成「横排也常显」，用户就**无法表达「只覆盖一侧」**，而那
//! 恰恰是分档最常见的用法。

use std::fmt;

use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// 档位名：横排。
pub const DIM_H: &str = "h";
/// 档位名：竖排。
pub const DIM_V: &str = "v";

/// 标签式语法认得的全部档位名（含全称别名）。
///
/// ★ **这张表就是「歧义判据」的全部依据**：一个字符串只有在 `冒号前的 token 命中本表`
/// 时才按分档解析，否则整串当标量值。故 `"C:\\path"`、`"18:00"`、`"note: see below"`
/// 这类本身含冒号的值不会被误读——`C` / `18` / `note` 都不在表里。
///
/// # ⚠️ 「拼错档位名」由值域校验兜底，不在这一层报错
///
/// `"x:hide"`（把 `v` 打成了 `x`）与 `"C:\\path"`（某个键的合法路径值）在语法上**完全
/// 同形**，本层无从分辨——想让前者报错，就必然误伤后者。
///
/// 故这里只认「语法歧义」，把「值合不合法」留给值域校验：`"x:hide"` 落成标量后必然不在
/// 该键的 `Enum` 值域里，那一层会报「值不在值域内」，用户照样看得到错误。⇒ 两个目标靠
/// **分层**同时达成，而不是在本层二选一。
///
/// 只有**部分命中**（`"h:hide x:always"`——显然是分档意图却写错了一个档位名）才由本层
/// 报错：那种形态不可能是某个键的合法标量值，不存在误伤。
///
/// ⛔ 往表里加档位名前先想一遍：新名字会不会撞上某个配置项的**合法标量值**的前缀。
/// 这是本语法唯一的真实风险点，值域越宽的键越要当心（加 `file`、`http` 这类词尤其危险）。
const DIM_NAMES: &[(&str, Dim)] = &[
    (DIM_H, Dim::H),
    (DIM_V, Dim::V),
    ("horizontal", Dim::H),
    ("vertical", Dim::V),
];

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Dim {
    H,
    V,
}

/// 按排布分档的值。
///
/// 形状与语义见模块文档。取值走 [`Self::get`]——**签名强制调用方给出排布**，这正是
/// 本类型相对「拆两个键」的要害：漏掉排布不是漏读一个键（静默取错值），而是编译不过。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ByLayout<T> {
    /// 标量写法：两种排布共用同一个值。
    Both(T),
    /// 分档写法：各配一份，缺侧＝该类型默认值（见模块文档「缺侧语义」）。
    Split { h: Option<T>, v: Option<T> },
}

impl<T> ByLayout<T> {
    /// 取当前排布该用的值；该档没写则 `None`（**不回退另一侧**，理由见模块文档）。
    pub fn get(&self, vertical: bool) -> Option<&T> {
        match self {
            Self::Both(v) => Some(v),
            Self::Split { h, v } => {
                if vertical {
                    v.as_ref()
                } else {
                    h.as_ref()
                }
            }
        }
    }

    /// 是否分档写法。设置页据此决定「两个控件还是一个」。
    pub fn is_split(&self) -> bool {
        matches!(self, Self::Split { .. })
    }
}

impl<T: Default> Default for ByLayout<T> {
    fn default() -> Self {
        Self::Both(T::default())
    }
}

impl ByLayout<String> {
    /// 取当前排布该用的字符串；该档没写则空串（这些键的空串恒表示「不覆盖」）。
    pub fn as_str(&self, vertical: bool) -> &str {
        self.get(vertical).map(String::as_str).unwrap_or("")
    }

    /// 两档各自的值，供**下发给渲染端**用。
    ///
    /// ★ 为什么下发两份而不是在这边 `select` 完再发：排布是渲染端的状态（随
    /// `set_orientation` 变，还有旋转态），协调器这边发出去的那一刻并不知道渲染端稍后会
    /// 用哪一档。发两份、由渲染端按自己的 `vertical` 取，与既有的 `min_window_*`
    /// 一次下发四个值是同一套做法。
    pub fn both_str(&self) -> (&str, &str) {
        (self.as_str(false), self.as_str(true))
    }
}

/// 标签式解析结果：`(横排档, 竖排档)`，各自可缺。
type TagResult = Result<(Option<String>, Option<String>), TagError>;

/// 解析标签式；不是标签式则返回 `None`（由调用方当标量处理）。
///
/// 判据见 [`DIM_NAMES`]。整串必须**全部**由 `档位名:值` 组成才算标签式——半个像半个不像
/// 的（`"h:hide 随便写点别的"`）一律不认，退回标量，宁可让用户看到「这个值不在值域里」
/// 的报错，也不要猜一半丢一半。
fn parse_tagged(s: &str) -> Option<TagResult> {
    let toks: Vec<&str> = s.split_whitespace().collect();
    if toks.is_empty() {
        return None;
    }
    // 先判定「这串到底是不是标签式」：至少一个 token 命中档位名，且**每个** token 都带冒号。
    let any_dim = toks.iter().any(|t| {
        t.split_once(':')
            .is_some_and(|(k, _)| DIM_NAMES.iter().any(|(n, _)| *n == k))
    });
    if !any_dim {
        return None;
    }
    let mut h = None;
    let mut v = None;
    for t in toks {
        let Some((key, val)) = t.split_once(':') else {
            // 混了个不带冒号的 token：不是合法标签式，且**不能**当标量放过——
            // 那会把 "h:hide 忘了写冒号" 静默当成一个普通值。
            return Some(Err(TagError::Malformed(t.to_string())));
        };
        let Some((_, dim)) = DIM_NAMES.iter().find(|(n, _)| *n == key) else {
            // ★ 未知档位名必须报错，不能忽略：忽略的话 "x:hide" 这种拼错会静默变成
            // 「没配过」，而用户明明写了东西——本仓最怕的静默失效形态。
            return Some(Err(TagError::UnknownDim(key.to_string())));
        };
        let slot = match dim {
            Dim::H => &mut h,
            Dim::V => &mut v,
        };
        if slot.is_some() {
            return Some(Err(TagError::Duplicate(key.to_string())));
        }
        *slot = Some(val.to_string());
    }
    Some(Ok((h, v)))
}

/// 把一个配置值摊成「每一档的实际取值」，供值域校验逐档比对。
///
/// 三种写法都摊得开：标量（一档）、标签式（写了几档就几个）、表（同理）。类型不对
/// （数字、数组…）返回 `None`，由调用方报类型错。
///
/// ★ 空串一律**不产出**：它恒表示「这一档不覆盖」，值域里未必收录空串，摊出来反而会
/// 让「只覆盖一侧」误报越界。
///
/// ⚠️ 摊平**故意不报**「档位名不认识」：`"x:hide"` 与某些键的合法标量值同形，这里分辨
/// 不了（见 [`DIM_NAMES`] 的文档）。它会原样作为一个标量值摊出来，然后在值域比对那一步
/// 报「不在值域内」——错误照样看得见，且路径类的值不被误伤。
pub fn domain_parts(value: &toml::Value) -> Option<Vec<String>> {
    let mut out = Vec::new();
    match value {
        toml::Value::String(s) => match parse_tagged(s) {
            // 不是标签式（含「整串看着像但档位名不认识」）⇒ 整串就是那一个值。
            None | Some(Err(_)) => out.push(s.clone()),
            Some(Ok((h, v))) => out.extend([h, v].into_iter().flatten()),
        },
        toml::Value::Table(m) => {
            for (k, val) in m {
                if DIM_NAMES.iter().any(|(n, _)| n == k)
                    && let Some(s) = val.as_str()
                {
                    out.push(s.to_string());
                }
            }
        }
        _ => return None,
    }
    out.retain(|s| !s.is_empty());
    Some(out)
}

/// 标签式语法错误。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TagError {
    /// 有 token 不是 `档位名:值` 形态。
    Malformed(String),
    /// 档位名不认识。
    UnknownDim(String),
    /// 同一档位写了两次。
    Duplicate(String),
}

impl fmt::Display for TagError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Malformed(t) => write!(f, "「{t}」不是「档位:值」形态"),
            Self::UnknownDim(k) => {
                write!(
                    f,
                    "不认识的排布档位「{k}」，可用：h/v（或 horizontal/vertical）"
                )
            }
            Self::Duplicate(k) => write!(f, "排布档位「{k}」写了不止一次"),
        }
    }
}

impl<'de> Deserialize<'de> for ByLayout<String> {
    fn deserialize<D>(de: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        // 与主题 `Ld` 同款：先落成 `toml::Value` 再 match。serde 的 `untagged` 在 toml
        // 上对「标量或表」这种形状支持得并不干净，而手写这一段还能顺带做容错。
        let v = toml::Value::deserialize(de)?;
        match v {
            toml::Value::String(s) => match parse_tagged(&s) {
                None => Ok(Self::Both(s)),
                Some(Ok((h, v))) => Ok(Self::Split { h, v }),
                Some(Err(e)) => Err(serde::de::Error::custom(e.to_string())),
            },
            toml::Value::Table(m) => {
                let get = |k: &str| m.get(k).and_then(|x| x.as_str()).map(str::to_string);
                // 表写法也认全称别名，与标签式保持同一套档位名。
                Ok(Self::Split {
                    h: get(DIM_H).or_else(|| get("horizontal")),
                    v: get(DIM_V).or_else(|| get("vertical")),
                })
            }
            // 其余类型（数字、数组…）：本仓配置解析一律容错不 panic，落成空标量即
            // 「没配」，由值域校验那一层去报「类型不对」。
            _ => Ok(Self::Both(String::new())),
        }
    }
}

impl Serialize for ByLayout<String> {
    fn serialize<S>(&self, ser: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        match self {
            Self::Both(s) => ser.serialize_str(s),
            Self::Split { h, v } => {
                // ⚠️ 值里有空格或冒号时标签式会自伤（空格是 token 分隔符），改用表写法——
                // 代价是写回后裂成子段，但那也好过写出一个自己都解析不回来的字符串。
                // 本类型当前的使用面（枚举型覆盖键）值域里没有这种值，恒走标签式。
                let unsafe_for_tags = [h, v]
                    .iter()
                    .filter_map(|x| x.as_deref())
                    .any(|s| s.contains(' ') || s.contains(':'));
                if unsafe_for_tags {
                    use serde::ser::SerializeMap;
                    let mut m = ser.serialize_map(None)?;
                    if let Some(h) = h {
                        m.serialize_entry(DIM_H, h)?;
                    }
                    if let Some(v) = v {
                        m.serialize_entry(DIM_V, v)?;
                    }
                    return m.end();
                }
                let mut out = String::new();
                // 空串＝「不覆盖」，与「没写这一档」同义 ⇒ 省略，免得写出 "h: v:always"。
                for (name, val) in [(DIM_H, h), (DIM_V, v)] {
                    if let Some(s) = val.as_deref().filter(|s| !s.is_empty()) {
                        if !out.is_empty() {
                            out.push(' ');
                        }
                        out.push_str(name);
                        out.push(':');
                        out.push_str(s);
                    }
                }
                ser.serialize_str(&out)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Serialize, Deserialize, Debug, Default, PartialEq)]
    #[serde(default)]
    struct Holder {
        x: ByLayout<String>,
        after: String,
    }

    fn de(s: &str) -> Holder {
        toml::from_str(s).expect("应能解析")
    }

    #[test]
    fn scalar_is_shared_by_both_layouts() {
        let h = de(r#"x = "hide""#);
        assert_eq!(h.x.as_str(false), "hide");
        assert_eq!(h.x.as_str(true), "hide");
        assert!(!h.x.is_split());
    }

    #[test]
    fn tagged_splits_the_two_layouts() {
        let h = de(r#"x = "h:hide v:always""#);
        assert_eq!(h.x.as_str(false), "hide");
        assert_eq!(h.x.as_str(true), "always");
        assert!(h.x.is_split());
    }

    #[test]
    fn one_sided_leaves_the_other_at_default() {
        // 「只覆盖一侧」是分档最常见的用法，缺侧必须是默认值（＝不覆盖），
        // 不能像主题 Ld 那样回退另一侧——否则这个用法根本表达不出来。
        let h = de(r#"x = "v:always""#);
        assert_eq!(h.x.as_str(true), "always");
        assert_eq!(h.x.as_str(false), "", "横排没写 ⇒ 不覆盖，不是跟着竖排走");
    }

    #[test]
    fn full_dim_names_are_accepted() {
        let h = de(r#"x = "horizontal:hide vertical:always""#);
        assert_eq!((h.x.as_str(false), h.x.as_str(true)), ("hide", "always"));
    }

    #[test]
    fn table_form_is_accepted_too() {
        let h = de(r#"x = { h = "hide", v = "always" }"#);
        assert_eq!((h.x.as_str(false), h.x.as_str(true)), ("hide", "always"));
        let full = de(r#"x = { horizontal = "hide", vertical = "always" }"#);
        assert_eq!(full.x, h.x, "表写法的全称别名应与缩写等价");
    }

    // ── 三条防护 ──────────────────────────────────────────────

    #[test]
    fn partially_unknown_dim_is_an_error_not_silently_ignored() {
        // 明显是分档意图却写错一个档位名 ⇒ 本层报错（不可能是谁的合法标量值，不误伤）。
        // 忽略的话，那一档会静默变成「没配过」，而用户明明写了东西。
        let e = toml::from_str::<Holder>(r#"x = "h:hide x:always""#).unwrap_err();
        assert!(e.to_string().contains("排布档位"), "实得：{e}");
    }

    #[test]
    fn wholly_unknown_dim_falls_through_to_value_domain_check() {
        // ★ "x:hide" 与 "C:\path" 语法同形，本层分辨不了，故一律当标量放过——
        // 它必然不在该键的 Enum 值域里，由值域校验那一层报错（见 DIM_NAMES 文档）。
        // 这条钉住的是「本层不自作聪明」，不是「拼错没人管」。
        let h = de(r#"x = "x:hide""#);
        assert!(!h.x.is_split());
        assert_eq!(h.x.as_str(false), "x:hide", "原样留给值域校验去判");
    }

    #[test]
    fn malformed_token_is_an_error() {
        let e = toml::from_str::<Holder>(r#"x = "h:hide always""#).unwrap_err();
        assert!(e.to_string().contains("档位"), "实得：{e}");
    }

    #[test]
    fn duplicate_dim_is_an_error() {
        let e = toml::from_str::<Holder>(r#"x = "h:hide h:always""#).unwrap_err();
        assert!(e.to_string().contains("不止一次"), "实得：{e}");
    }

    #[test]
    fn values_containing_colons_are_not_mistaken_for_tags() {
        // ★ 歧义判据：冒号前的 token 不在档位名单里 ⇒ 整串当标量。
        for raw in [
            r#"x = "C:\\path\\to""#,
            r#"x = "18:00""#,
            r#"x = "note: see""#,
        ] {
            let h = de(raw);
            assert!(!h.x.is_split(), "不该被当成分档语法：{raw}");
        }
    }

    // ── 序列化：形态不漂移 ────────────────────────────────────

    #[test]
    fn scalar_round_trips_unchanged() {
        let h = de(r#"
x = "hide"
after = "z"
"#);
        let out = toml::to_string(&h).unwrap();
        assert!(out.contains(r#"x = "hide""#), "标量往返应原样：{out}");
        assert_eq!(de(&out).x, h.x);
    }

    #[test]
    fn split_serializes_as_a_tag_string_not_a_subtable() {
        // 这条是本 wire format 存在的全部理由：表写法会被 to_string 写成子段
        // `[x]`，把同段后面的标量字段挤到前面去；标签式永远是标量，形态不漂移。
        let h = de(r#"
x = { h = "hide", v = "always" }
after = "z"
"#);
        let out = toml::to_string(&h).unwrap();
        assert!(!out.contains("[x]"), "不该裂出子段：{out}");
        assert!(out.contains(r#"x = "h:hide v:always""#), "实得：{out}");
        // 往返闭合
        assert_eq!(de(&out).x, h.x);
    }

    #[test]
    fn empty_side_is_omitted_from_the_tag_string() {
        let h = de(r#"x = { v = "always" }"#);
        let out = toml::to_string(&h).unwrap();
        assert!(out.contains(r#"x = "v:always""#), "实得：{out}");
    }

    #[test]
    fn values_with_spaces_fall_back_to_table_form() {
        // 值里有空格时标签式会自伤，改用表写法（接受子段化）。
        let h = ByLayout::Split {
            h: Some("a b".to_string()),
            v: Some("c".to_string()),
        };
        let out = toml::to_string(&Holder {
            x: h.clone(),
            after: String::new(),
        })
        .unwrap();
        assert!(out.contains("[x]"), "含空格应回退表写法：{out}");
        assert_eq!(de(&out).x, h, "表写法仍须往返闭合");
    }
}
