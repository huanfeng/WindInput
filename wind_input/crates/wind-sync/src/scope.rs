//! 同步范围：用户目录默认全同步，由分类表排除本机项（数据/资源/设置/排除）。

use serde::{Deserialize, Serialize};

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
        ["config.toml"]
        | ["compat.toml"]
        | ["wind_setting.toml"]
        | ["wind_setting_hide.toml"]
        | ["schema_overrides", ..] => Kind::Setting,
        ["data", schema, "dicts.toml"] if *schema != "charsets" => Kind::Setting,
        ["themes", ..] | ["extensions", ..] => Kind::PerClass,
        _ => Kind::Shared,
    }
}

/// 路径的分类：数据与资源默认同步，设置按组同步，排除项只留本机。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Class {
    Data,
    Resource,
    Setting,
    Excluded,
}

pub fn classify(local: &str) -> Class {
    let segs: Vec<&str> = local.split('/').collect();
    let last = segs.last().copied().unwrap_or("");
    if segs.iter().any(|s| s.starts_with('.'))
        || last.ends_with(".tmp")
        || last.contains(".bak-")
        || last.contains(".conflict-")
        || last.ends_with(".sys_hash")
        || [".redb", ".db", ".wjl", ".sealed", ".legacy", ".lock"]
            .iter()
            .any(|x| last.ends_with(x))
    {
        return Class::Excluded;
    }
    match segs.as_slice() {
        ["meta.toml", ..] | ["screenshots", ..] | ["charsets", ..] => return Class::Excluded,
        // 与远端命名空间前缀同名：能上传却映射不回来
        ["desktop" | "mobile" | "groups", ..] => return Class::Excluded,
        ["data", "stats", ..] => return Class::Excluded,
        ["data", ..]
            if matches!(
                last,
                "temp.dict.yaml"
                    | "freq.table.yaml"
                    | "assoc.table.yaml"
                    | "draft.dict.yaml"
                    | "completion.table.yaml"
            ) =>
        {
            return Class::Excluded;
        }
        _ => {}
    }
    if kind(local) == Kind::Setting {
        return Class::Setting;
    }
    match segs.as_slice() {
        ["schemas", ..] | ["themes", ..] | ["extensions", ..] | ["system.softkeyboard.toml"] => {
            Class::Resource
        }
        _ => Class::Data,
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
pub fn in_scope_remote(remote: &str, ns: &Namespace) -> bool {
    to_local(remote, ns).is_some_and(|l| in_scope(&l))
}

/// 远端逻辑路径是否是该组的设置文件。
pub fn is_group_setting(remote: &str, group: &str) -> bool {
    remote
        .strip_prefix("groups/")
        .and_then(|r| r.split_once('/'))
        .is_some_and(|(g, local)| g == group && kind(local) == Kind::Setting)
}

/// `path` 为用户目录下以 `/` 分隔的相对路径；合法且不属于排除项才同步。
pub fn in_scope(path: &str) -> bool {
    if path.is_empty()
        || path.starts_with('/')
        || path.contains('\\')
        || path
            .split('/')
            .any(|s| s.is_empty() || s == "." || s == "..")
    {
        return false;
    }
    classify(path) != Class::Excluded
}

#[cfg(test)]
mod tests {
    use super::*;

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
        assert!(in_scope_remote("groups/home/config.toml", &home));
        assert!(!in_scope_remote("groups/office/config.toml", &home));
        assert!(!in_scope_remote("../config.toml", &home));
        assert!(!in_scope_remote("groups/home/../config.toml", &home));
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
            "data/pinyin/dicts.toml",
            "emoji/a.txt",
            "other.txt",
            "wind_setting.toml",
        ] {
            assert!(in_scope(p), "{p}");
        }
    }

    #[test]
    fn excluded() {
        for p in [
            "meta.toml",
            "data/.backup/x.yaml",
            "data/phrases.sys_hash",
            "data/pinyin/temp.dict.yaml",
            "data/stats/a.toml",
            "schemas/.x.tmp",
            "themes/a.tmp",
            "config.conflict-a-2026-11-24.toml",
            "../config.toml",
            "/config.toml",
            "data//phrases.dict.yaml",
            "a\\b",
        ] {
            assert!(!in_scope(p), "{p}");
        }
    }

    #[test]
    fn kind_covers_settings_and_per_class() {
        assert_eq!(kind("schema_overrides/x.toml"), Kind::Setting);
        assert_eq!(kind("wind_setting.toml"), Kind::Setting);
        assert_eq!(kind("wind_setting_hide.toml"), Kind::Setting);
        assert_eq!(kind("extensions/k/a"), Kind::PerClass);
        assert_eq!(kind("themes/a"), Kind::PerClass);
        assert_eq!(kind("emoji/a.txt"), Kind::Shared);
    }
}
