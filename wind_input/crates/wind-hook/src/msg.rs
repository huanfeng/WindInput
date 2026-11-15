//! 线上消息。核心 ↔ 宿主、宿主 ↔ 扩展用同一套；宿主转发时只裁剪，不改结构。
//!
//! JSON 形如 `{"t":"key","v":1,"seq":3,...}`：`t` 是类型，`v`/`seq`/`dropped` 由 [`crate::frame`]
//! 在帧这一层加上，消息本身不带。未知 `t` 解成 [`Msg::Unknown`]，未知字段忽略。

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "t", rename_all = "snake_case")]
pub enum Msg {
    // ── 握手（扩展 ↔ 宿主） ──
    /// 扩展 → 宿主：支持的协议版本、想要的档位。身份由宿主按拉起关系确定，不自报。
    Hello {
        #[serde(default)]
        protocol: Vec<u32>,
        #[serde(default)]
        want: Vec<String>,
    },
    /// 宿主 → 扩展：选定的协议版本与实际授予的档位。
    Welcome {
        protocol: u32,
        granted: Vec<String>,
        core: String,
    },
    /// 扩展 → 宿主：收窄订阅（事件类型名，见 [`Msg::event_kind`]）。
    Subscribe {
        #[serde(default)]
        events: Vec<String>,
    },
    Ping,
    Pong,

    // ── 事件（核心 → 宿主 → 扩展） ──
    Key(KeyEvent),
    Mode(ModeEvent),
    Composition(CompositionEvent),
    Candidates(CandidatesEvent),
    Commit(CommitEvent),

    // ── 控制（核心 ↔ 宿主） ──
    /// 核心 → 宿主：启用的扩展与各自的授权。启动时与配置变更时下发，宿主据此增量启停。
    Config {
        #[serde(default)]
        extensions: Vec<ExtensionGrant>,
    },
    /// 宿主 → 扩展：扩展选项的完整取值（已按清单校验、补齐默认值）。握手后发一次，之后变化时再发。
    Options {
        #[serde(default)]
        values: serde_json::Map<String, serde_json::Value>,
    },
    /// 核心 → 宿主：停掉全部扩展后退出。
    Shutdown,
    /// 宿主 → 核心：各扩展的运行状态，状态变化时上报。
    Status {
        #[serde(default)]
        extensions: Vec<ExtensionStatus>,
    },

    /// 不认识的类型（对端版本更新）。收到即丢弃。
    #[serde(other)]
    Unknown,
}

impl Msg {
    /// 事件类型名（订阅用）；非事件消息为 `None`。
    pub fn event_kind(&self) -> Option<&'static str> {
        match self {
            Msg::Key(_) => Some("key"),
            Msg::Mode(_) => Some("mode"),
            Msg::Composition(_) => Some("composition"),
            Msg::Candidates(_) => Some("candidates"),
            Msg::Commit(_) => Some("commit"),
            _ => None,
        }
    }
}

/// 按键按下。`key` 是物理键名（小写，如 `a`、`1`、`space`、`semicolon`），只有 `key.detail`
/// 档才有；`class` 是分类，所有按键档都有。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct KeyEvent {
    /// 核心单调时钟毫秒。扩展据此丢弃过期事件。
    pub ts: u64,
    /// 组合会话 id；无组合为 0。
    #[serde(default)]
    pub sid: u64,
    /// 应用进程名，可为空。
    #[serde(default)]
    pub app: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub mods: Vec<String>,
    pub class: String,
    /// 输入法是否吃掉了这个键。
    #[serde(default)]
    pub handled: bool,
    /// 输入法本不处理、前端为扩展旁路上报的键。
    #[serde(default)]
    pub observed: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ModeEvent {
    pub ts: u64,
    pub chinese: bool,
    pub full_width: bool,
    #[serde(default)]
    pub schema: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CompositionEvent {
    pub ts: u64,
    #[serde(default)]
    pub sid: u64,
    #[serde(default)]
    pub app: String,
    pub code: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CandidatesEvent {
    pub ts: u64,
    #[serde(default)]
    pub sid: u64,
    #[serde(default)]
    pub app: String,
    #[serde(default)]
    pub page: u32,
    #[serde(default)]
    pub selected: u32,
    pub items: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CommitEvent {
    pub ts: u64,
    #[serde(default)]
    pub sid: u64,
    #[serde(default)]
    pub app: String,
    pub text: String,
    /// 来源：`candidate` / `command` / `punct` / `softkeyboard`。
    #[serde(default)]
    pub source: String,
    /// 组合矩形 `[x, y, w, h]`，屏幕物理像素；应用不报时省略。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rect: Option<[i32; 4]>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ExtensionGrant {
    pub id: String,
    #[serde(default)]
    pub granted: Vec<String>,
    /// 用户设过的扩展选项（未校验，宿主按清单校验）。
    #[serde(default, skip_serializing_if = "serde_json::Map::is_empty")]
    pub options: serde_json::Map<String, serde_json::Value>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExtensionStatus {
    pub id: String,
    /// `running` / `starting` / `backoff` / `tripped` / `invalid` / `stopped`。
    pub state: String,
    #[serde(default)]
    pub restarts: u32,
    #[serde(default)]
    pub dropped: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unknown_type_decodes_as_unknown() {
        let m: Msg = serde_json::from_str(r#"{"t":"suggest","sid":3,"items":[]}"#).unwrap();
        assert_eq!(m, Msg::Unknown);
    }

    #[test]
    fn unknown_fields_are_ignored() {
        let m: Msg =
            serde_json::from_str(r#"{"t":"mode","ts":5,"chinese":true,"full_width":false,"x":1}"#)
                .unwrap();
        assert!(matches!(
            m,
            Msg::Mode(ModeEvent {
                ts: 5,
                chinese: true,
                ..
            })
        ));
    }

    #[test]
    fn key_event_omits_absent_detail() {
        let m = Msg::Key(KeyEvent {
            ts: 1,
            sid: 0,
            app: String::new(),
            key: None,
            mods: vec![],
            class: "space".into(),
            handled: false,
            observed: false,
        });
        let s = serde_json::to_string(&m).unwrap();
        assert!(s.starts_with(r#"{"t":"key""#), "{s}");
        assert!(!s.contains("\"key\":null") && !s.contains("mods"), "{s}");
    }
}
