//! 同步范围（设计 §8）：四类开关 + 包含/排除清单。

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScopeFlags {
    /// config.toml、compat.toml
    pub config: bool,
    /// schemas/
    pub schemas: bool,
    /// themes/
    pub themes: bool,
    /// 词库与短语（data/ 下的文本词库、短语、字符集）
    pub dicts: bool,
}

impl ScopeFlags {
    pub const ALL: ScopeFlags = ScopeFlags {
        config: true,
        schemas: true,
        themes: true,
        dicts: true,
    };
}

impl Default for ScopeFlags {
    fn default() -> Self {
        Self::ALL
    }
}

/// `path` 为用户目录下以 `/` 分隔的相对路径。
pub fn in_scope(path: &str, f: &ScopeFlags) -> bool {
    if path.is_empty()
        || path.starts_with('/')
        || path.contains('\\')
        || path
            .split('/')
            .any(|s| s.is_empty() || s == "." || s == "..")
    {
        return false;
    }
    let segs: Vec<&str> = path.split('/').collect();
    match segs.as_slice() {
        ["config.toml"] | ["compat.toml"] => f.config,
        ["schemas", ..] => f.schemas && !hidden(&segs),
        ["themes", ..] => f.themes && !hidden(&segs),
        ["data", rest @ ..] => f.dicts && data_in_scope(rest),
        _ => false,
    }
}

/// 隐藏文件与临时文件（同目录临时文件 + rename 的中间态）不同步。
fn hidden(segs: &[&str]) -> bool {
    segs.iter().any(|s| s.starts_with('.')) || segs.last().is_some_and(|s| s.ends_with(".tmp"))
}

fn data_in_scope(rest: &[&str]) -> bool {
    if hidden(rest) {
        return false; // 含 data/.backup/
    }
    match rest {
        ["phrases.dict.yaml"] | ["quick_format.toml"] | ["common_chars.yaml"] => true,
        ["charsets", _, ..] => true,
        [schema, "user.dict.yaml"] | [schema, "shadow.dict.yaml"] | [schema, "dicts.toml"] => {
            *schema != "charsets"
        }
        [schema, "dicts", _, ..] => *schema != "charsets",
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const A: ScopeFlags = ScopeFlags::ALL;

    #[test]
    fn included() {
        for p in [
            "config.toml",
            "compat.toml",
            "schemas/pinyin.schema.yaml",
            "themes/a/theme.json",
            "data/phrases.dict.yaml",
            "data/quick_format.toml",
            "data/common_chars.yaml",
            "data/charsets/gbk.txt",
            "data/pinyin/user.dict.yaml",
            "data/pinyin/shadow.dict.yaml",
            "data/pinyin/dicts.toml",
            "data/pinyin/dicts/x.dict.yaml",
        ] {
            assert!(in_scope(p, &A), "{p}");
        }
    }

    #[test]
    fn excluded() {
        for p in [
            "meta.toml",
            "data/.backup/x.yaml",
            "data/phrases.sys_hash",
            "local/state/x",
            "data/pinyin/temp.wjl",
            "data/pinyin/freq.redb",
            "data/pinyin/assoc.db",
            "data/pinyin/draft",
            "data/pinyin/stats",
            "data/pinyin/completion",
            "schemas/.x.tmp",
            "themes/a.tmp",
            "../config.toml",
            "/config.toml",
            "data//phrases.dict.yaml",
            "other.txt",
        ] {
            assert!(!in_scope(p, &A), "{p}");
        }
    }

    #[test]
    fn flags_gate_each_class() {
        let none = ScopeFlags {
            config: false,
            schemas: false,
            themes: false,
            dicts: false,
        };
        assert!(!in_scope("config.toml", &none));
        assert!(in_scope(
            "config.toml",
            &ScopeFlags {
                config: true,
                ..none
            }
        ));
        assert!(in_scope(
            "themes/a",
            &ScopeFlags {
                themes: true,
                ..none
            }
        ));
        assert!(!in_scope(
            "themes/a",
            &ScopeFlags {
                schemas: true,
                ..none
            }
        ));
        assert!(in_scope(
            "schemas/a",
            &ScopeFlags {
                schemas: true,
                ..none
            }
        ));
        assert!(in_scope(
            "data/phrases.dict.yaml",
            &ScopeFlags {
                dicts: true,
                ..none
            }
        ));
        assert!(!in_scope(
            "data/phrases.dict.yaml",
            &ScopeFlags {
                config: true,
                ..none
            }
        ));
    }
}
