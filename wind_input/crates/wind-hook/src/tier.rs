//! 授权档位：扩展能收到哪些事件、收到多细。
//!
//! 裁剪分两级（设计 §4.2）：核心按所有已启用扩展的授权**并集**决定产出什么；宿主再按每个
//! 扩展自己的授权裁剪一次。两级用的是同一个 [`trim`]。

use crate::msg::Msg;

/// 一个授权档位。线上名见 [`Tier::name`]。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Tier {
    /// 按键分类，不含具体键。
    KeyClass,
    /// 具体物理键名 + 修饰键。隐含 `KeyClass`。
    KeyDetail,
    /// 中英、全半角、方案切换。
    Mode,
    /// 当前编码串。
    Composition,
    /// 当前页候选文本。
    Candidates,
    /// 上屏文本。
    Commit,
    /// 预留给候选注入，v1 一律不授予。
    CandidateProvide,
}

impl Tier {
    pub const ALL: [Tier; 7] = [
        Tier::KeyClass,
        Tier::KeyDetail,
        Tier::Mode,
        Tier::Composition,
        Tier::Candidates,
        Tier::Commit,
        Tier::CandidateProvide,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Tier::KeyClass => "key.class",
            Tier::KeyDetail => "key.detail",
            Tier::Mode => "mode",
            Tier::Composition => "composition",
            Tier::Candidates => "candidates",
            Tier::Commit => "commit",
            Tier::CandidateProvide => "candidate.provide",
        }
    }

    pub fn parse(s: &str) -> Option<Tier> {
        Tier::ALL.into_iter().find(|t| t.name() == s)
    }

    fn bit(self) -> u8 {
        1 << (self as u8)
    }
}

/// 档位集合（位图）。可在线程间以 `u8` 原子共享，见 [`crate::tap::Tap`]。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Grants(u8);

impl Grants {
    pub const NONE: Grants = Grants(0);

    pub fn from_bits(bits: u8) -> Grants {
        Grants(bits)
    }

    pub fn bits(self) -> u8 {
        self.0
    }

    /// 从线上名解析；不认识的名字跳过。`candidate.provide` 在 v1 被剔除——
    /// 不管配置里写了什么，都授予不出去。
    pub fn parse<'a>(names: impl IntoIterator<Item = &'a str>) -> Grants {
        let mut g = Grants::NONE;
        for t in names.into_iter().filter_map(Tier::parse) {
            if t != Tier::CandidateProvide {
                g = g.with(t);
            }
        }
        g
    }

    /// 加一个档位。`key.detail` 隐含 `key.class`：申请细档、只获授粗档的扩展仍应收到分类。
    pub fn with(self, t: Tier) -> Grants {
        let g = Grants(self.0 | t.bit());
        if t == Tier::KeyDetail {
            g.with(Tier::KeyClass)
        } else {
            g
        }
    }

    pub fn has(self, t: Tier) -> bool {
        self.0 & t.bit() != 0
    }

    pub fn union(self, o: Grants) -> Grants {
        Grants(self.0 | o.0)
    }

    pub fn intersect(self, o: Grants) -> Grants {
        Grants(self.0 & o.0)
    }

    pub fn is_empty(self) -> bool {
        self.0 == 0
    }

    /// 能否收到按键事件（任一按键档位）。
    pub fn has_key(self) -> bool {
        self.has(Tier::KeyClass) || self.has(Tier::KeyDetail)
    }

    pub fn names(self) -> Vec<String> {
        Tier::ALL
            .into_iter()
            .filter(|t| self.has(*t))
            .map(|t| t.name().to_string())
            .collect()
    }
}

/// 按授权裁剪一条消息：没有对应档位就返回 `None`；按键只有 `key.class` 时抹掉具体键与修饰键。
/// 非事件消息（握手、控制）原样放行——它们不由档位管。
pub fn trim(msg: &Msg, grants: Grants) -> Option<Msg> {
    match msg {
        Msg::Key(k) => {
            if grants.has(Tier::KeyDetail) {
                Some(msg.clone())
            } else if grants.has(Tier::KeyClass) {
                let mut k = k.clone();
                k.key = None;
                k.mods.clear();
                Some(Msg::Key(k))
            } else {
                None
            }
        }
        Msg::Mode(_) => grants.has(Tier::Mode).then(|| msg.clone()),
        Msg::Composition(_) => grants.has(Tier::Composition).then(|| msg.clone()),
        Msg::Candidates(_) => grants.has(Tier::Candidates).then(|| msg.clone()),
        Msg::Commit(_) => grants.has(Tier::Commit).then(|| msg.clone()),
        _ => Some(msg.clone()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::msg::{CommitEvent, KeyEvent};

    fn key() -> Msg {
        Msg::Key(KeyEvent {
            ts: 1,
            sid: 0,
            app: String::new(),
            key: Some("a".into()),
            mods: vec!["shift".into()],
            class: "letter".into(),
            handled: true,
            observed: false,
        })
    }

    #[test]
    fn names_round_trip() {
        for t in Tier::ALL {
            assert_eq!(Tier::parse(t.name()), Some(t));
        }
        assert_eq!(Tier::parse("bogus"), None);
    }

    #[test]
    fn candidate_provide_cannot_be_granted() {
        let g = Grants::parse(["candidate.provide", "mode"]);
        assert!(!g.has(Tier::CandidateProvide));
        assert!(g.has(Tier::Mode));
    }

    #[test]
    fn key_class_only_strips_key_and_mods() {
        let Some(Msg::Key(k)) = trim(&key(), Grants::NONE.with(Tier::KeyClass)) else {
            panic!("key.class 应收到按键");
        };
        assert_eq!(k.key, None);
        assert!(k.mods.is_empty());
        assert_eq!(k.class, "letter");
    }

    #[test]
    fn key_detail_implies_key_class() {
        let want = Grants::parse(["key.detail"]);
        let granted = Grants::parse(["key.class"]);
        let eff = want.intersect(granted);
        assert!(eff.has(Tier::KeyClass) && !eff.has(Tier::KeyDetail));
    }

    #[test]
    fn key_detail_keeps_everything() {
        assert_eq!(
            trim(&key(), Grants::NONE.with(Tier::KeyDetail)),
            Some(key())
        );
    }

    #[test]
    fn ungranted_events_are_dropped() {
        let commit = Msg::Commit(CommitEvent {
            ts: 1,
            sid: 0,
            app: String::new(),
            text: "你".into(),
            source: "candidate".into(),
            rect: None,
        });
        let g = Grants::NONE.with(Tier::KeyDetail).with(Tier::Mode);
        assert_eq!(trim(&commit, g), None);
        assert_eq!(trim(&key(), Grants::NONE.with(Tier::Mode)), None);
    }

    #[test]
    fn control_messages_pass_through() {
        assert_eq!(trim(&Msg::Ping, Grants::NONE), Some(Msg::Ping));
    }
}
