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

/// 设备类别：决定平台相关文件（配置、主题、兼容）在远端的命名空间。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DeviceClass {
    #[default]
    Desktop,
    Mobile,
}

impl DeviceClass {
    /// 远端路径前缀（也是 `parse` 接受的名字）。
    pub fn as_str(self) -> &'static str {
        match self {
            DeviceClass::Desktop => "desktop",
            DeviceClass::Mobile => "mobile",
        }
    }

    pub fn parse(s: &str) -> Option<DeviceClass> {
        match s {
            "desktop" => Some(DeviceClass::Desktop),
            "mobile" => Some(DeviceClass::Mobile),
            _ => None,
        }
    }
}

/// 平台相关文件：配置、兼容规则、主题。其余（词库、短语、方案）各类别共用。
fn platform_specific(local: &str) -> bool {
    matches!(
        local.split('/').next(),
        Some("config.toml" | "compat.toml" | "themes")
    )
}

/// 本机路径 → 远端逻辑路径：平台相关文件带类别前缀，共用文件原样。
pub fn to_remote(local: &str, class: DeviceClass) -> String {
    if platform_specific(local) {
        format!("{}/{local}", class.as_str())
    } else {
        local.to_string()
    }
}

/// 远端逻辑路径 → 本机路径。别的类别的平台相关条目、没有前缀的平台相关条目返回 `None`（忽略）。
pub fn to_local(remote: &str, class: DeviceClass) -> Option<String> {
    if let Some((head, rest)) = remote.split_once('/')
        && let Some(c) = DeviceClass::parse(head)
    {
        return (c == class && platform_specific(rest)).then(|| rest.to_string());
    }
    (!platform_specific(remote)).then(|| remote.to_string())
}

/// 远端逻辑路径是否属于本机类别且在同步范围内。
pub fn in_scope_remote(remote: &str, f: &ScopeFlags, class: DeviceClass) -> bool {
    to_local(remote, class).is_some_and(|l| in_scope(&l, f))
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
    fn remote_mapping_namespaces_platform_files() {
        use DeviceClass::*;
        assert_eq!(to_remote("config.toml", Mobile), "mobile/config.toml");
        assert_eq!(
            to_remote("themes/a/t.toml", Desktop),
            "desktop/themes/a/t.toml"
        );
        assert_eq!(to_remote("compat.toml", Desktop), "desktop/compat.toml");
        assert_eq!(
            to_remote("data/phrases.dict.yaml", Mobile),
            "data/phrases.dict.yaml"
        );
        assert_eq!(
            to_remote("schemas/a.schema.yaml", Mobile),
            "schemas/a.schema.yaml"
        );
        assert_eq!(
            to_local("mobile/config.toml", Mobile).as_deref(),
            Some("config.toml")
        );
        assert_eq!(to_local("desktop/config.toml", Mobile), None);
        assert_eq!(to_local("config.toml", Mobile), None, "无前缀的旧条目忽略");
        assert_eq!(to_local("themes/a", Desktop), None);
        assert_eq!(to_local("data/x", Mobile).as_deref(), Some("data/x"));
        assert_eq!(to_local("mobile/data/x", Mobile), None, "共用文件不带前缀");
        assert!(in_scope_remote("desktop/config.toml", &A, Desktop));
        assert!(!in_scope_remote("mobile/config.toml", &A, Desktop));
        assert!(!in_scope_remote("../config.toml", &A, Desktop));
        assert!(!in_scope_remote("desktop/../config.toml", &A, Desktop));
    }

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
