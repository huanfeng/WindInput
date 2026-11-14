//! 编进产物的内置主题（`data/themes/`）：编辑器里的主题 `base = "default"` 之类要能找到它们，
//! 与 core 按目录加载时同一份文本；图片资产（`_base` 的翻页箭头）登记进内存资源表。
//!
//! 内置主题有增删时这里跟着改——`lists_every_builtin_theme_and_asset` 会红。

/// `(主题 id, theme.toml 文本)`。
const THEMES: &[(&str, &str)] = &[
    (
        "_base",
        include_str!("../../../../data/themes/_base/theme.toml"),
    ),
    (
        "_qingfeng",
        include_str!("../../../../data/themes/_qingfeng/theme.toml"),
    ),
    (
        "amber",
        include_str!("../../../../data/themes/amber/theme.toml"),
    ),
    (
        "default",
        include_str!("../../../../data/themes/default/theme.toml"),
    ),
    (
        "jade",
        include_str!("../../../../data/themes/jade/theme.toml"),
    ),
    (
        "msime",
        include_str!("../../../../data/themes/msime/theme.toml"),
    ),
    (
        "violet",
        include_str!("../../../../data/themes/violet/theme.toml"),
    ),
];

/// `(主题 id, 文件名, 字节)`：主题里按文件名字面引用（`ref = "chevron_prev.svg"`）。
const ASSETS: &[(&str, &str, &[u8])] = &[
    (
        "_base",
        "chevron_next.svg",
        include_bytes!("../../../../data/themes/_base/chevron_next.svg"),
    ),
    (
        "_base",
        "chevron_prev.svg",
        include_bytes!("../../../../data/themes/_base/chevron_prev.svg"),
    ),
];

/// 未调 `set_theme` 时预览用的主题。
pub const DEFAULT_THEME: &str = "default";

/// 内置主题的 theme.toml 文本。
pub fn theme_text(id: &str) -> Option<String> {
    THEMES
        .iter()
        .find(|(i, _)| *i == id)
        .map(|(_, t)| (*t).to_string())
}

/// 内置主题 id（含 `_` 开头的抽象 base）。
pub fn theme_ids() -> impl Iterator<Item = &'static str> {
    THEMES.iter().map(|(i, _)| *i)
}

/// 内置图片资产登记进 `m`（调用方的资源随后登记，同名覆盖内置）。
pub fn add_assets(m: &mut wind_scene::assets::MemoryAssets) {
    for (_, name, bytes) in ASSETS {
        m.insert(name, *bytes);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 与 `data/themes/` 逐个对齐：漏编一个内置主题，用户主题 `base` 到它就在编辑器里报找不到。
    #[test]
    fn lists_every_builtin_theme_and_asset() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../data/themes");
        let ids = wind_theme::list_theme_ids(&root);
        assert_eq!(theme_ids().collect::<Vec<_>>(), ids);
        let mut files = Vec::new();
        for id in &ids {
            for e in std::fs::read_dir(root.join(id)).unwrap() {
                let name = e.unwrap().file_name().to_string_lossy().into_owned();
                if name != "theme.toml" {
                    files.push((id.clone(), name));
                }
            }
        }
        files.sort();
        let listed: Vec<_> = ASSETS
            .iter()
            .map(|(i, n, _)| (i.to_string(), n.to_string()))
            .collect();
        assert_eq!(listed, files);
        for id in ids {
            let text = theme_text(&id).unwrap();
            let base = |b: &str| theme_text(b).ok_or_else(|| anyhow::anyhow!("没有 {b}"));
            wind_theme::load_typed_text(&text, &id, &base).unwrap();
        }
    }
}
