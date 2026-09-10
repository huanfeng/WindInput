//! 缓存文件的**命名空间目录**：源文件在 `schemas/` 下的相对目录链。
//!
//! 词库缓存（`.wdat`/`.wridx`）与注释库缓存（`.wcmt`）都不能直接把文件名干扔进缓存根
//! ——不同方案下的同名文件会撞在一起。历史实现用「源文件的**直接父目录名**」做命名空间，
//! 因为出厂结构恒是扁平的 `schemas/<方案>/x.dict.yaml`，那个等式碰巧成立。
//!
//! # 为什么不能停在「父目录名」
//!
//! 词库路径解析是 `layer.path.join(rel)`，`rel` 由方案自己声明，**多深都收**。用户一旦
//! 写成 `ime_wubi86/word/x.dict.yaml`，父目录名就变成了 `word`，方案那一段在推导中整个
//! 丢失：缓存落到 `<cache>/word/`，与 `<cache>/ime_wubi86/` 同级（论坛 #115）。于是
//!
//! - 两个方案各有 `word/常用词.dict.yaml` 时缓存路径**逐字节相同**，互相顶掉对方的
//!   缓存反复全量重建（内容指纹保住了正确性，代价是每次切换方案都重解析）；
//! - 「一个方案的缓存归拢一处、整方案失效＝删一个目录」这个设计意图也随之失效。
//!
//! 改成保留 `schemas` 之后的**完整目录链**，上面两条同时消解，而扁平结构算出来的路径
//! 与旧实现逐字节一致——绝大多数用户不会因此重建任何缓存。
//!
//! # 为什么是「路径里找 schemas」而不是「把 rel 传进来」
//!
//! `rel` 语义上更准，但 `EngineManager` 有三个调用点（combined 合并、merged、wridx）
//! 手里只有解析后的绝对路径，沿调用链补参数要动一串签名；更要命的是这会让缓存路径不再是
//! **源路径的纯函数**——`reader_pool` 的复用契约（同一文件必得同一 key）与 pinyin /
//! shuangpin / 混输子引擎共用同一份 `merged.wdat` 的有意设计，都建立在这条纯函数性上。
//! 三层（data / user / custom）的词库根恒是 `<层根>/schemas`（见 `dict_layers` 的
//! `.sub("schemas")`），从路径里回认这一段既够用又不破坏纯函数性。

use std::ffi::OsStr;
use std::path::{Component, Path, PathBuf};

/// 各层词库根的固定目录名。
const SCHEMAS_DIR_NAME: &str = "schemas";

/// 源文件 → 缓存命名空间（相对目录链，可为空＝直接落缓存根）。
///
/// - `…/schemas/wubi86/x.dict.yaml` → `wubi86`（与旧实现一致）
/// - `…/schemas/ime_wubi86/word/x.dict.yaml` → `ime_wubi86/word`（本次修复）
/// - `…/schemas/x.dict.yaml` → 空（词库直接躺在 schemas 根，无从分组）
/// - 路径里没有 `schemas` 段 → 回退到父目录名（测试夹具、便携版自定义根）
///
/// 只收 [`Component::Normal`] 段：`..`/盘符/根不会进入结果，缓存因此不可能被源路径里的
/// `../` 或 `X:name` 带出缓存根。
pub fn schema_namespace(source: &Path) -> PathBuf {
    let Some(dir) = source.parent() else {
        return PathBuf::new();
    };
    let segs: Vec<&OsStr> = dir
        .components()
        .filter_map(|c| match c {
            Component::Normal(s) => Some(s),
            _ => None,
        })
        .collect();

    // 取**最后一个** `schemas`：用户数据根本身可能也叫这个名字，靠后的那个才是词库根。
    match segs
        .iter()
        .rposition(|s| s.eq_ignore_ascii_case(SCHEMAS_DIR_NAME))
    {
        Some(i) => segs[i + 1..].iter().collect(),
        // 没有 schemas 段：退回旧行为（父目录名），免得测试夹具与便携版的缓存路径失去分组。
        None => segs.last().map(PathBuf::from).unwrap_or_default(),
    }
}

/// 命名空间 + 文件名 → 缓存路径。命名空间为空时直接落在 `cache_root` 下。
pub fn cache_path_in(cache_root: &Path, source: &Path, file_name: &str) -> PathBuf {
    let ns = schema_namespace(source);
    if ns.as_os_str().is_empty() {
        cache_root.join(file_name)
    } else {
        cache_root.join(ns).join(file_name)
    }
}

/// 文件名干：剥掉 `.dict.yaml` 里 `.dict` 这个冗余中缀（`rime_frost.dict` → `rime_frost`）。
pub fn cache_stem(source: &Path) -> String {
    let stem = source
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    stem.strip_suffix(".dict").unwrap_or(&stem).to_string()
}

/// ⚠️ 用例里的路径字面量一律写成 `/` 分隔的**相对**路径。
///
/// 反斜杠只在 Windows 上是分隔符：`r"C:\d\schemas\p\x.yaml"` 到了 Linux / macOS 是
/// **一整个** `Component::Normal`，`schemas` 那一段根本切不出来，于是断言的左边恒是空串
/// ——本仓的 CI（Linux、macOS 都真跑 test）为此红过一整轮，而本机 Windows 全绿。
/// 正斜杠两个平台都认，故只用它；Windows 独有的分隔语义由 `windows_backslash_layout`
/// 单独兜住。
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flat_layout_keeps_legacy_path() {
        // 出厂扁平结构：与旧的「父目录名」实现逐字节一致，存量缓存不失效。
        assert_eq!(
            schema_namespace(Path::new("d/data/schemas/wubi86/wubi86_jidian.dict.yaml")),
            PathBuf::from("wubi86")
        );
    }

    #[test]
    fn nested_layout_keeps_schema_segment() {
        // 论坛 #115：二级子目录下，方案那一段必须留在命名空间里。
        let ns = schema_namespace(Path::new(
            "d/data/schemas/ime_wubi86/word/changyong.dict.yaml",
        ));
        assert_eq!(ns, PathBuf::from("ime_wubi86").join("word"));
    }

    #[test]
    fn same_leaf_dir_across_schemas_does_not_collide() {
        // 用户真正担心的那个冲突：两个方案各有 word/ 且文件同名。
        let a = cache_path_in(
            Path::new("c"),
            Path::new("d/data/schemas/ime_wubi86/word/x.dict.yaml"),
            "x.wdat",
        );
        let b = cache_path_in(
            Path::new("c"),
            Path::new("d/data/schemas/ime_wubi98/word/x.dict.yaml"),
            "x.wdat",
        );
        assert_ne!(a, b);
    }

    #[test]
    fn user_layer_and_data_layer_share_one_cache() {
        // 同一 rel 在不同层里只会有一个胜出者，共用一份缓存是有意设计（内容指纹判新鲜）。
        let user = schema_namespace(Path::new("Users/u/AppData/Roaming/W/schemas/p/a.dict.yaml"));
        let data = schema_namespace(Path::new("Program Files/W/data/schemas/p/a.dict.yaml"));
        assert_eq!(user, data);
    }

    #[test]
    fn last_schemas_segment_wins() {
        // 数据根自己也叫 schemas 时，靠后的那个才是词库根。
        assert_eq!(
            schema_namespace(Path::new("schemas/data/schemas/pinyin/rime.dict.yaml")),
            PathBuf::from("pinyin")
        );
    }

    #[test]
    fn dict_at_schemas_root_falls_to_cache_root() {
        assert_eq!(
            schema_namespace(Path::new("d/data/schemas/x.dict.yaml")),
            PathBuf::new()
        );
    }

    #[test]
    fn without_schemas_segment_falls_back_to_parent_name() {
        // 测试夹具/便携版：没有 schemas 段时保持旧行为，仍按父目录名分组。
        assert_eq!(
            schema_namespace(Path::new("tmp/fixture/wubi86/x.dict.yaml")),
            PathBuf::from("wubi86")
        );
    }

    #[test]
    fn parent_traversal_segments_are_dropped() {
        // `..` 不进命名空间：缓存写不出缓存根。
        let ns = schema_namespace(Path::new("d/data/schemas/p/../q/x.dict.yaml"));
        assert_eq!(ns, PathBuf::from("p").join("q"));
        assert!(!ns.to_string_lossy().contains(".."));
    }

    /// Windows 上反斜杠也是分隔符：盘符段被 `Component::Prefix` / `RootDir` 挡在外面，
    /// 命名空间与正斜杠写法逐字节一致。
    #[test]
    #[cfg(windows)]
    fn windows_backslash_layout() {
        assert_eq!(
            schema_namespace(Path::new(r"C:\d\data\schemas\ime_wubi86\word\x.dict.yaml")),
            PathBuf::from("ime_wubi86").join("word")
        );
    }

    #[test]
    fn stem_strips_dict_infix() {
        assert_eq!(
            cache_stem(Path::new("a/rime_frost.dict.yaml")),
            "rime_frost"
        );
        assert_eq!(cache_stem(Path::new("a/en.yaml")), "en");
    }
}
