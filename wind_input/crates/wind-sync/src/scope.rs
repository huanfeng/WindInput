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

/// 文件在远端的归属：数据各设备共享，主题按设备类别，设置按设备组。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Shared,
    PerClass,
    Setting,
}

pub fn kind(local: &str) -> Kind {
    let segs: Vec<&str> = local.split('/').collect();
    match segs.as_slice() {
        ["config.toml"] | ["compat.toml"] => Kind::Setting,
        ["data", schema, "dicts.toml"] if *schema != "charsets" => Kind::Setting,
        ["themes", ..] => Kind::PerClass,
        _ => Kind::Shared,
    }
}

/// 本机所在的远端命名空间：类别（主题）与设备组（设置）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Namespace {
    pub class: DeviceClass,
    pub group: String,
}

impl Namespace {
    pub fn new(class: DeviceClass, group: impl Into<String>) -> Self {
        Namespace {
            class,
            group: group.into(),
        }
    }
}

/// 本机路径 → 远端逻辑路径：设置带 `groups/<组>/`，主题带类别前缀，数据原样。
pub fn to_remote(local: &str, ns: &Namespace) -> String {
    match kind(local) {
        Kind::Setting => format!("groups/{}/{local}", ns.group),
        Kind::PerClass => format!("{}/{local}", ns.class.as_str()),
        Kind::Shared => local.to_string(),
    }
}

/// 远端逻辑路径 → 本机路径。别组的设置、别类别的主题、带前缀的数据、无前缀的设置/主题返回 `None`。
pub fn to_local(remote: &str, ns: &Namespace) -> Option<String> {
    if let Some((head, rest)) = remote.split_once('/') {
        if head == "groups" {
            let (group, local) = rest.split_once('/')?;
            return (group == ns.group && kind(local) == Kind::Setting).then(|| local.to_string());
        }
        if let Some(c) = DeviceClass::parse(head) {
            return (c == ns.class && kind(rest) == Kind::PerClass).then(|| rest.to_string());
        }
    }
    (kind(remote) == Kind::Shared).then(|| remote.to_string())
}

/// 远端逻辑路径是否属于本机命名空间且在同步范围内。
pub fn in_scope_remote(remote: &str, f: &ScopeFlags, ns: &Namespace) -> bool {
    to_local(remote, ns).is_some_and(|l| in_scope(&l, f))
}

/// 远端逻辑路径是否是该组的设置文件。
pub fn is_group_setting(remote: &str, group: &str) -> bool {
    remote
        .strip_prefix("groups/")
        .and_then(|r| r.split_once('/'))
        .is_some_and(|(g, local)| g == group && kind(local) == Kind::Setting)
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

    fn ns(class: &str, group: &str) -> Namespace {
        Namespace::new(DeviceClass::parse(class).unwrap(), group)
    }

    #[test]
    fn remote_mapping_namespaces_by_kind() {
        let home = ns("desktop", "home");
        assert_eq!(to_remote("config.toml", &home), "groups/home/config.toml");
        assert_eq!(
            to_remote("data/wubi86/dicts.toml", &home),
            "groups/home/data/wubi86/dicts.toml"
        );
        assert_eq!(
            to_remote("themes/a/t.toml", &home),
            "desktop/themes/a/t.toml"
        );
        assert_eq!(
            to_remote("data/wubi86/user.dict.yaml", &home),
            "data/wubi86/user.dict.yaml"
        );
        assert_eq!(
            to_remote("data/charsets/dicts.toml", &home),
            "data/charsets/dicts.toml"
        );
        assert_eq!(
            to_local("groups/home/config.toml", &home).as_deref(),
            Some("config.toml")
        );
        assert_eq!(to_local("groups/office/config.toml", &home), None);
        assert_eq!(
            to_local("groups/home/data/wubi86/user.dict.yaml", &home),
            None,
            "数据不允许带组前缀"
        );
        assert_eq!(to_local("config.toml", &home), None, "无前缀的设置忽略");
        assert_eq!(
            to_local("desktop/themes/a", &home).as_deref(),
            Some("themes/a")
        );
        assert_eq!(to_local("mobile/themes/a", &home), None);
        assert_eq!(to_local("desktop/data/x", &home), None, "数据不带类别前缀");
        assert_eq!(to_local("data/x", &home).as_deref(), Some("data/x"));
        assert!(in_scope_remote("groups/home/config.toml", &A, &home));
        assert!(!in_scope_remote("groups/office/config.toml", &A, &home));
        assert!(!in_scope_remote("../config.toml", &A, &home));
        assert!(!in_scope_remote("groups/home/../config.toml", &A, &home));
    }

    #[test]
    fn group_setting_detection() {
        assert!(is_group_setting("groups/home/config.toml", "home"));
        assert!(!is_group_setting("groups/home/config.toml", "office"));
        assert!(!is_group_setting(
            "groups/home/data/x/user.dict.yaml",
            "home"
        ));
        assert!(!is_group_setting("config.toml", "home"));
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
