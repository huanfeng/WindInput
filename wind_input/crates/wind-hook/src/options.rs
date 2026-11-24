// SPDX-License-Identifier: MPL-2.0
//! 扩展选项（设计见 `docs/design/extension-hook-channel.md` §6.4）：扩展在清单里声明，设置端照声明
//! 渲染表单，值存核心配置，宿主按声明校验、补齐默认值后发给扩展。
//!
//! ```toml
//! [[option]]
//! key = "theme"
//! label = "音色"
//! type = "choice"                  # choice / bool / number
//! default = "click"
//! choices = [{ value = "click", label = "机械键盘" }, { value = "piano", label = "钢琴" }]
//!
//! [[option]]
//! key = "volume"
//! label = "音量"
//! type = "number"
//! default = 0.6
//! min = 0.0
//! max = 1.0
//! step = 0.05
//! ```

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OptionSpec {
    pub key: String,
    #[serde(default)]
    pub label: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub description: String,
    #[serde(flatten)]
    pub kind: OptionKind,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum OptionKind {
    Choice {
        default: String,
        choices: Vec<Choice>,
    },
    Bool {
        default: bool,
    },
    Number {
        default: f64,
        min: f64,
        max: f64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        step: Option<f64>,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Choice {
    pub value: String,
    #[serde(default)]
    pub label: String,
}

impl OptionSpec {
    fn default_value(&self) -> Value {
        match &self.kind {
            OptionKind::Choice { default, .. } => Value::from(default.as_str()),
            OptionKind::Bool { default } => Value::from(*default),
            OptionKind::Number { default, .. } => Value::from(*default),
        }
    }

    /// 用户值合法就用（数值夹到范围内），否则取默认值。
    fn accept(&self, v: Option<&Value>) -> Value {
        let ok = match (&self.kind, v) {
            (OptionKind::Choice { choices, .. }, Some(Value::String(s))) => choices
                .iter()
                .any(|c| &c.value == s)
                .then(|| Value::from(s.as_str())),
            (OptionKind::Bool { .. }, Some(Value::Bool(b))) => Some(Value::from(*b)),
            (OptionKind::Number { min, max, .. }, Some(Value::Number(n))) => {
                n.as_f64().map(|x| Value::from(x.clamp(*min, *max)))
            }
            _ => None,
        };
        ok.unwrap_or_else(|| self.default_value())
    }
}

/// 校验清单里的选项声明。写错了整份清单无效——这是扩展作者的错，要在第一次加载时就报出来。
pub fn validate(specs: &[OptionSpec]) -> Result<(), String> {
    let mut seen = std::collections::HashSet::new();
    for s in specs {
        if !crate::manifest::valid_id(&s.key) {
            return Err(format!(
                "选项键 `{}` 不合法（只许小写字母、数字、-、_）",
                s.key
            ));
        }
        if !seen.insert(s.key.as_str()) {
            return Err(format!("选项键 `{}` 重复", s.key));
        }
        match &s.kind {
            OptionKind::Choice { default, choices } => {
                if !choices.iter().any(|c| &c.value == default) {
                    return Err(format!(
                        "选项 `{}` 的默认值 `{default}` 不在可选值里",
                        s.key
                    ));
                }
            }
            OptionKind::Number {
                default, min, max, ..
            } => {
                if !(min <= default && default <= max) {
                    return Err(format!(
                        "选项 `{}` 的默认值 {default} 不在 {min}~{max} 内",
                        s.key
                    ));
                }
            }
            OptionKind::Bool { .. } => {}
        }
    }
    Ok(())
}

/// 发给扩展的完整取值：每个声明的选项都有值；用户值不合法的、清单没声明的都不进来。
pub fn resolve(specs: &[OptionSpec], user: &Map<String, Value>) -> Map<String, Value> {
    specs
        .iter()
        .map(|s| (s.key.clone(), s.accept(user.get(&s.key))))
        .collect()
}

#[cfg(test)]
#[path = "options_tests.rs"]
mod tests;
