//! 按候选窗**排布**分档的配置值：标量＝横竖共用，分档＝各配一份。
//!
//! 横排与竖排的可用空间差一个数量级，一批配置项的合理值不同。用一个字段类型表达，
//! 而不是拆成 `_horizontal` / `_vertical` 两个键：
//!
//! ```toml
//! pager_bar_display = "hide"              # 横竖共用
//! pager_bar_display = "h:hide v:always"   # 横排隐藏、竖排常显
//! pager_bar_display = "v:always"          # 只说竖排；横排＝默认（此处即「跟随主题」）
//! pager_bar_display = { h = "hide", v = "always" }   # 表写法，等价
//! ```
//!
//! 标量写法始终合法，老配置零迁移。
//!
//! # 与主题 `Ld` 同构，但序列化与缺侧语义不同
//!
//! 概念与 API 照搬 [`wind_theme::schema::Ld`]：`select(vertical)` 对应 `select(is_dark)`，
//! 解析同样手写 `Deserialize`、容错不 panic。
//!
//! 序列化默认输出标签式：配置会被程序整表写回，而 TOML 表写法写回会裂成子段、打乱同段
//! 标量的顺序（主题不写回故无此问题）。两种写法都读得懂。
//!
//! 缺一侧回退该类型的默认值（「不覆盖」），而不像 `Ld` 回退另一侧：否则 `"v:always"`
//! 会变成横排也常显，用户无法表达「只覆盖一侧」。

use std::fmt;

use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// 档位名：横排。
pub const DIM_H: &str = "h";
/// 档位名：竖排。
pub const DIM_V: &str = "v";

/// 标签式语法认得的全部档位名（含全称别名）。
///
/// 这张表是歧义判据的全部依据：只有冒号前的 token 命中本表才按分档解析，否则整串当标量，
/// 故 `"C:\\path"`、`"18:00"` 不会被误读。
///
/// 拼错档位名（`"x:hide"`）与合法路径值同形，本层不报错，落成标量后由值域校验报「不在值域
/// 内」。只有部分命中（`"h:hide x:always"`）才由本层报错，那不可能是合法标量。
///
/// 加档位名前先确认它不会撞上某个配置项合法标量值的前缀（`file`、`http` 这类尤其危险）。
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
    /// 下发两份而不在这边 `select`：排布是渲染端状态（随 `set_orientation` 与旋转变），
    /// 协调器发出时不知道渲染端会用哪一档。同 `min_window_*` 的做法。
    pub fn both_str(&self) -> (&str, &str) {
        (self.as_str(false), self.as_str(true))
    }
}

/// 标签式解析结果：`(横排档, 竖排档)`，各自可缺。
type TagResult = Result<(Option<String>, Option<String>), TagError>;

/// 解析标签式；不是标签式则返回 `None`（由调用方当标量处理）。
///
/// 判据见 [`DIM_NAMES`]。整串必须全部由 `档位名:值` 组成才算标签式，否则退回标量，
/// 让值域校验报错，不猜一半丢一半。
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
            // 未知档位名必须报错，不能忽略：忽略的话 "x:hide" 这种拼错会静默变成
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
/// 空串不产出：它表示「这一档不覆盖」，摊出来会让「只覆盖一侧」误报越界。
/// 不认识的档位名不在这里报，原样作为标量摊出，由值域比对报错（见 [`DIM_NAMES`]）。
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
                // 值里有空格或冒号时标签式会自伤（空格是 token 分隔符），改用表写法——
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
#[path = "by_layout_tests.rs"]
mod tests;
