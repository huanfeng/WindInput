//! 学习数据（临时词/词频/联想/补全）的同步逻辑（设计 docs/design/sync-learned-data.md §4）。

pub mod format;
pub mod model;
pub mod table;

use std::collections::BTreeMap;

/// 学习文件种类。
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub enum LearnKind {
    Temp,
    Freq,
    Assoc,
    Completion,
}

impl LearnKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Temp => "temp",
            Self::Freq => "freq",
            Self::Assoc => "assoc",
            Self::Completion => "completion",
        }
    }

    pub fn file_name(self) -> &'static str {
        match self {
            Self::Temp => "temp.dict.yaml",
            Self::Freq => "freq.table.yaml",
            Self::Assoc => "assoc.table.yaml",
            Self::Completion => "completion.table.yaml",
        }
    }

    pub fn key_cols(self) -> [&'static str; 2] {
        match self {
            Self::Temp | Self::Freq => ["code", "text"],
            Self::Assoc => ["context", "text"],
            Self::Completion => ["kind", "text"],
        }
    }

    pub fn field_cols(self) -> &'static [&'static str] {
        match self {
            Self::Temp => &["weight", "created_at"],
            _ => &["last_used"],
        }
    }
}

/// 世代号：先比毫秒，同值按设备号字典序；默认值是初始世代。
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Debug, Default)]
pub struct Gen {
    pub ms: u64,
    pub dev: String,
}

impl Gen {
    pub fn initial() -> Gen {
        Gen::default()
    }

    /// 新世代 = max(本机毫秒, 当前毫秒 + 1)。
    pub fn next(cur: &Gen, now_ms: u64, dev: &str) -> Gen {
        Gen {
            ms: now_ms.max(cur.ms.saturating_add(1)),
            dev: dev.to_string(),
        }
    }

    /// 解析 `0` 或 `<ms>:<dev>`。
    pub fn parse(s: &str) -> Option<Gen> {
        if s == "0" {
            return Some(Gen::initial());
        }
        let (ms, dev) = s.split_once(':')?;
        if dev.is_empty() {
            return None;
        }
        Some(Gen {
            ms: ms.parse().ok()?,
            dev: dev.to_string(),
        })
    }

    pub fn render(&self) -> String {
        if *self == Gen::initial() {
            "0".to_string()
        } else {
            format!("{}:{}", self.ms, self.dev)
        }
    }
}

/// 原始键字段（转义后的原文）。
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Key(pub String, pub String);

pub type NormKey = (String, String);

/// temp/freq 的 code 去全部空白，其余原样。
pub fn norm_key(kind: LearnKind, k: &Key) -> NormKey {
    match kind {
        LearnKind::Temp | LearnKind::Freq => (
            k.0.chars().filter(|c| !c.is_whitespace()).collect(),
            k.1.clone(),
        ),
        _ => (k.0.clone(), k.1.clone()),
    }
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Row {
    pub gen_: Gen,
    pub count: u64,
    /// 顺序同 [`LearnKind::field_cols`]。
    pub fields: Vec<String>,
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Contribution {
    pub kind: LearnKind,
    pub epoch: Gen,
    /// Key 保留首次见到的原样写法。
    pub rows: BTreeMap<NormKey, (Key, Row)>,
    pub tombs: BTreeMap<NormKey, (Key, Gen)>,
}
