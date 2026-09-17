//! 主题包（`.wtheme`）—— 主题的 zip 分发格式。
//!
//! ```text
//! package.toml      元信息：format_version / kind / name / author / version（**不落盘**）
//! theme.toml        主题本体（根条目，识别主题的依据）
//! preview.png       市场预览图（可选，**不落盘**）
//! assets/bg.png     主题引用的图片等资源
//! ```
//!
//! ## 为什么不与方案包（`.wpkg`）共用一个后缀
//!
//! 扩展名是**路由判据**：设置端靠它分派意图、靠它给 ProgId 挂图标与「打开方式」名称，
//! 关联的注册/解除开关也是 per-扩展名的。合成一个后缀的话，路由得先解压才知道手上
//! 是什么，错误面从「这不是主题包」扩成「包坏了 / 类型不对 / 解压失败」三种。
//!
//! 但扩展名只是**提示**——文件随时能被改名，权威判据是包内 `package.toml` 的 `kind`。
//! 没有 `package.toml` 的包（手工打的）退回看根目录有没有 `theme.toml`，与 `.wpkg`
//! 容忍手工打包同一套取舍。
//!
//! ## 只落该落的
//!
//! `package.toml` 与 `preview.png` 按名识别、**不写进主题目录**：它们不是主题资源，
//! 落盘只会在每个主题目录里多两个死文件。同 [`crate::scheme`] 对 `config_patch.toml`
//! 的处理。

use std::io::Write;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// 包内元信息文件名。
pub const THEME_META_NAME: &str = "package.toml";
/// 主题本体条目名（根条目）。
pub const THEME_ENTRY_NAME: &str = "theme.toml";
/// 市场预览图条目名（可选）。
pub const THEME_PREVIEW_NAME: &str = "preview.png";
/// 资源目录前缀。
pub const THEME_ASSETS_PREFIX: &str = "assets/";
/// `package.toml` 里标识「这是主题包」的取值。
pub const THEME_KIND: &str = "theme";

/// 当前实现支持的主题包格式版本（写进导出的 package.toml，导入按此门禁）。
pub const THEME_FORMAT_VERSION: u32 = 1;

/// 包内条目数上限。
///
/// 真实主题包是「一个 theme.toml + 几张图」，几十个条目顶天。比方案包的 2000 严得多：
/// 那边要给「一个包带多套混输子方案 + 全量词库」留余量，主题没有那种形态。
pub const MAX_THEME_ENTRIES: usize = 200;

/// 包内解压后**总**字节上限。
///
/// 主题图片是候选窗尺寸的位图，单张几百 KB 顶天；32 MiB 对任何真实主题都绰绰有余，
/// 同时把 zip 炸弹挡在把服务进程撑爆之前。
pub const MAX_THEME_UNCOMPRESSED_BYTES: u64 = 32 * 1024 * 1024;

/// `theme.toml` / `package.toml` 这类**文本**条目的单条上限。
///
/// 单独给一个小上限，免得走总额那条时要先把几十 MB 读进来才发现不对。取值对齐设置端
/// 下载主题的 2 MiB 再留一倍余量——历史上内嵌 base64 图片的主题正好在这个量级。
const MAX_THEME_TEXT_BYTES: u64 = 4 * 1024 * 1024;

/// 缺 format_version 字段的包一律视为 legacy v1。
fn legacy_format_version() -> u32 {
    1
}

/// 主题包元信息（package.toml）。全部字段可缺省——拿不到就回退到主题自身的 meta。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ThemeMeta {
    #[serde(default)]
    pub package: ThemePackageInfo,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ThemePackageInfo {
    #[serde(default = "legacy_format_version")]
    pub format_version: u32,
    /// 包类型的**权威判据**。空 = 手工打包，退回按根条目推断。
    #[serde(default)]
    pub kind: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub author: String,
    #[serde(default)]
    pub version: String,
}

impl Default for ThemePackageInfo {
    fn default() -> Self {
        Self {
            format_version: legacy_format_version(),
            kind: String::new(),
            name: String::new(),
            author: String::new(),
            version: String::new(),
        }
    }
}

/// 包预览：够设置端弹「要导入这个主题吗」，不落任何盘。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ThemePreview {
    /// 展示名。优先 package.toml，退回 theme.toml 的 `meta.name`。
    pub display_name: String,
    pub author: String,
    pub version: String,
    /// 包内资源条目数（不含 theme.toml / package.toml / preview.png）。
    pub asset_count: usize,
    /// 包内是否带市场预览图。
    pub has_preview: bool,
}

/// 导入结果：落盘了哪些相对路径。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ThemeImportOutcome {
    pub display_name: String,
    /// 相对主题目录的落盘路径（`theme.toml` 与 `assets/**`）。
    pub files: Vec<String>,
}

/// 扫描结果：条目分类 + 元信息。**不读载荷**。
struct Scanned {
    meta: ThemeMeta,
    /// zip 原名 → 归一化相对路径。只含要落盘的条目。
    payload: Vec<(String, String)>,
    /// theme.toml 的 zip 原名。
    theme_entry: String,
    has_preview: bool,
}

/// 读 package.toml（缺失则全默认），并做版本门禁。
fn read_meta(package: &Path) -> anyhow::Result<ThemeMeta> {
    let raw = match crate::bundle::extract_entry_limited(
        package,
        THEME_META_NAME,
        MAX_THEME_TEXT_BYTES,
    ) {
        Ok(bytes) => bytes,
        // 手工打的包可以没有 package.toml，此时全默认（kind 空 → 按根条目推断）。
        Err(_) => return Ok(ThemeMeta::default()),
    };
    let text = String::from_utf8(raw)
        .map_err(|_| anyhow::anyhow!("{THEME_META_NAME} 不是有效 UTF-8 文本"))?;
    let meta: ThemeMeta =
        toml::from_str(&text).map_err(|e| anyhow::anyhow!("{THEME_META_NAME} 解析失败：{e}"))?;
    if meta.package.format_version > THEME_FORMAT_VERSION {
        anyhow::bail!(
            "主题包格式版本 {} 高于本程序支持的 {THEME_FORMAT_VERSION}，请升级清风输入法",
            meta.package.format_version
        );
    }
    // kind 认得出且不是主题 ⇒ 当场说清楚是什么，别让用户对着「缺 theme.toml」猜。
    if !meta.package.kind.is_empty() && meta.package.kind != THEME_KIND {
        anyhow::bail!("这是「{}」类型的包，不是主题包", meta.package.kind);
    }
    Ok(meta)
}

/// 扫描包：规模门禁 → 条目归类。不读载荷。
fn scan(package: &Path) -> anyhow::Result<Scanned> {
    let meta = read_meta(package)?;
    let file = std::fs::File::open(package)?;
    let mut archive = zip::ZipArchive::new(file)?;

    // 规模门禁只读**中央目录**，不解压任何内容，故排在逐条校验之前——限额要挡的正是
    // 「先把整个包处理一遍再说它太大」那份开销。
    //
    // ⚠️ 这里的「解压后大小」是归档自己声明的，撒谎的包骗得过它。真正的界画在
    // `import_package` 的逐条有界读取上，本段只是廉价前筛。
    if archive.len() > MAX_THEME_ENTRIES {
        anyhow::bail!(
            "主题包条目过多（{} 个，上限 {MAX_THEME_ENTRIES}）",
            archive.len()
        );
    }
    let mut declared: u64 = 0;
    for i in 0..archive.len() {
        declared = declared.saturating_add(archive.by_index_raw(i)?.size());
    }
    if declared > MAX_THEME_UNCOMPRESSED_BYTES {
        anyhow::bail!(
            "主题包解压后过大（声明 {declared} 字节，上限 {MAX_THEME_UNCOMPRESSED_BYTES}）"
        );
    }

    let mut payload: Vec<(String, String)> = Vec::new();
    let mut theme_entry: Option<String> = None;
    let mut has_preview = false;
    let names: Vec<String> = archive.file_names().map(String::from).collect();
    for name in &names {
        if name.ends_with('/') {
            continue;
        }
        // 判根与落盘一律用归一化值：`sub\theme.toml` 的 `contains('/')` 为 false，
        // 拿原名判根会把子目录文件当成根条目。校验同时挡住路径穿越。
        let rel = crate::bundle::validate_entry_rel(name, "")?;
        if rel == THEME_META_NAME {
            continue;
        }
        if rel == THEME_PREVIEW_NAME {
            has_preview = true;
            continue;
        }
        if rel == THEME_ENTRY_NAME {
            theme_entry = Some(name.clone());
            payload.push((name.clone(), rel));
            continue;
        }
        if let Some(rest) = rel.strip_prefix(THEME_ASSETS_PREFIX) {
            if rest.is_empty() {
                continue;
            }
            payload.push((name.clone(), rel));
            continue;
        }
        // 既不是约定条目也不在 assets/ 下：拒绝而不是忽略。悄悄丢掉的话，作者打包时
        // 把图放错位置，得等主题装进输入法、图不显示才发现。
        anyhow::bail!("主题包里有不认识的条目「{rel}」（资源要放在 {THEME_ASSETS_PREFIX} 下）");
    }

    let theme_entry = theme_entry
        .ok_or_else(|| anyhow::anyhow!("主题包缺少 {THEME_ENTRY_NAME}（它是识别主题的依据）"))?;
    Ok(Scanned {
        meta,
        payload,
        theme_entry,
        has_preview,
    })
}

/// 从 theme.toml 文本里取展示名/作者/版本，作为 package.toml 缺省时的回退。
fn meta_from_theme_text(text: &str) -> (String, String, String) {
    wind_theme::meta_from_text(text)
        .map(|m| (m.name, m.author, m.version))
        .unwrap_or_default()
}

/// 读包内 `theme.toml` 文本（有界）。
fn read_theme_text(package: &Path, entry: &str) -> anyhow::Result<String> {
    let bytes = crate::bundle::extract_entry_limited(package, entry, MAX_THEME_TEXT_BYTES)?;
    String::from_utf8(bytes).map_err(|_| anyhow::anyhow!("{THEME_ENTRY_NAME} 不是有效 UTF-8 文本"))
}

/// 包预览：读元信息与主题 meta，不落盘。
pub fn preview_package(package: &Path) -> anyhow::Result<ThemePreview> {
    let s = scan(package)?;
    let text = read_theme_text(package, &s.theme_entry)?;
    // 本体先过一遍合法性：预览阶段就说「这主题有问题」，比落盘后再回滚便宜。
    wind_theme::validate_text(&text)?;
    let (tname, tauthor, tversion) = meta_from_theme_text(&text);
    let p = &s.meta.package;
    let pick = |a: &str, b: String| {
        if a.is_empty() { b } else { a.to_string() }
    };
    Ok(ThemePreview {
        display_name: pick(&p.name, tname),
        author: pick(&p.author, tauthor),
        version: pick(&p.version, tversion),
        asset_count: s
            .payload
            .iter()
            .filter(|(_, rel)| rel != THEME_ENTRY_NAME)
            .count(),
        has_preview: s.has_preview,
    })
}

/// 解包到 `target_dir`（= `<主题目录>/<id>`）。
///
/// **整目录替换**而不是逐文件覆盖：资源可能改名，逐文件写会把上一版的图留成垃圾，
/// 而主题目录是按 id 独占的，没有「用户往里手工放了东西」这种用法。
/// 先写 `.tmp` 兄弟目录、成功后才动目标，中途失败目标原封不动。
pub fn import_package(package: &Path, target_dir: &Path) -> anyhow::Result<ThemeImportOutcome> {
    let s = scan(package)?;
    let text = read_theme_text(package, &s.theme_entry)?;
    wind_theme::validate_text(&text)?;
    let (tname, _, _) = meta_from_theme_text(&text);
    let display_name = if s.meta.package.name.is_empty() {
        tname
    } else {
        s.meta.package.name.clone()
    };

    // 读取阶段：全部载荷入内存，任何坏条目在写盘前失败。
    //
    // 逐条**有界**读取，且预算是整包共享的余额——`scan` 那道前筛读的是归档自己声明的
    // 大小，撒谎的包骗得过它（声明 1 KB、解压 10 GB）。
    let mut remaining = MAX_THEME_UNCOMPRESSED_BYTES;
    let mut staged: Vec<(String, Vec<u8>)> = Vec::new();
    for (name, rel) in &s.payload {
        let bytes = crate::bundle::extract_entry_limited(package, name, remaining)?;
        remaining = remaining.saturating_sub(bytes.len() as u64);
        staged.push((rel.clone(), bytes));
    }

    let tmp_dir = sibling_with_suffix(target_dir, ".wtheme-tmp")?;
    let _ = std::fs::remove_dir_all(&tmp_dir);
    let write_all = || -> anyhow::Result<()> {
        for (rel, bytes) in &staged {
            let path = tmp_dir.join(rel);
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            let mut f = std::fs::File::create(&path)?;
            f.write_all(bytes)?;
        }
        Ok(())
    };
    if let Err(e) = write_all() {
        let _ = std::fs::remove_dir_all(&tmp_dir);
        return Err(e);
    }

    // 就位：旧目录先挪开（作回滚用），tmp 顶上，最后才删旧的。
    // Windows 的 rename 到已存在目标会失败，故不能直接覆盖。
    let old_dir = sibling_with_suffix(target_dir, ".wtheme-old")?;
    let _ = std::fs::remove_dir_all(&old_dir);
    let had_old = target_dir.exists();
    if had_old {
        std::fs::rename(target_dir, &old_dir)?;
    }
    if let Some(parent) = target_dir.parent() {
        std::fs::create_dir_all(parent)?;
    }
    if let Err(e) = std::fs::rename(&tmp_dir, target_dir) {
        // 没能顶上就把旧的放回去，别让用户的主题凭空消失。
        if had_old {
            let _ = std::fs::rename(&old_dir, target_dir);
        }
        let _ = std::fs::remove_dir_all(&tmp_dir);
        return Err(e.into());
    }
    let _ = std::fs::remove_dir_all(&old_dir);

    Ok(ThemeImportOutcome {
        display_name,
        files: staged.into_iter().map(|(rel, _)| rel).collect(),
    })
}

/// `<dir>` → 同级的 `<dir><suffix>`。目标没有父目录时报错而不是静默落到当前目录。
fn sibling_with_suffix(dir: &Path, suffix: &str) -> anyhow::Result<PathBuf> {
    let name = dir
        .file_name()
        .ok_or_else(|| anyhow::anyhow!("主题目录路径不合法：{}", dir.display()))?;
    let mut with = name.to_os_string();
    with.push(suffix);
    Ok(dir.with_file_name(with))
}

/// 把一个主题目录打成 `.wtheme`。
///
/// 只收 `theme.toml` 与 `assets/**`——主题目录里可能还有别的东西（编辑器的草稿、
/// 系统生成的缩略图数据库），一并打进去既胀包又泄露无关文件。
pub fn export_package(
    theme_dir: &Path,
    out_path: &Path,
    preview_png: Option<&[u8]>,
) -> anyhow::Result<()> {
    let theme_path = theme_dir.join(THEME_ENTRY_NAME);
    let text = std::fs::read_to_string(&theme_path)
        .map_err(|e| anyhow::anyhow!("读不到 {}：{e}", theme_path.display()))?;
    wind_theme::validate_text(&text)?;
    let (name, author, version) = meta_from_theme_text(&text);

    let meta = ThemeMeta {
        package: ThemePackageInfo {
            format_version: THEME_FORMAT_VERSION,
            kind: THEME_KIND.to_string(),
            name,
            author,
            version,
        },
    };

    if let Some(parent) = out_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let file = std::fs::File::create(out_path)?;
    let mut w = zip::ZipWriter::new(file);
    let opts = zip::write::SimpleFileOptions::default();

    w.start_file(THEME_META_NAME, opts)?;
    w.write_all(toml::to_string_pretty(&meta)?.as_bytes())?;
    w.start_file(THEME_ENTRY_NAME, opts)?;
    w.write_all(text.as_bytes())?;
    if let Some(png) = preview_png {
        w.start_file(THEME_PREVIEW_NAME, opts)?;
        w.write_all(png)?;
    }

    let assets = theme_dir.join("assets");
    if assets.is_dir() {
        let mut files: Vec<PathBuf> = Vec::new();
        collect_files(&assets, &mut files)?;
        // 条目顺序稳定：同样的输入要打出同样的包，否则每次导出都是「全变了」。
        files.sort();
        for path in files {
            let rel = path
                .strip_prefix(&assets)
                .map_err(|_| anyhow::anyhow!("资源路径越界：{}", path.display()))?;
            let entry = format!(
                "{THEME_ASSETS_PREFIX}{}",
                rel.to_string_lossy().replace('\\', "/")
            );
            w.start_file(entry, opts)?;
            w.write_all(&std::fs::read(&path)?)?;
        }
    }
    w.finish()?;
    Ok(())
}

/// 递归收集目录下的文件（不含目录项）。
fn collect_files(dir: &Path, out: &mut Vec<PathBuf>) -> anyhow::Result<()> {
    for entry in std::fs::read_dir(dir)? {
        let path = entry?.path();
        if path.is_dir() {
            collect_files(&path, out)?;
        } else {
            out.push(path);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    const THEME_TEXT: &str = "[meta]\nname = \"水墨\"\nauthor = \"某人\"\nversion = \"1.2\"\n\n[window]\nbackground = { image = { ref = \"assets/bg.png\" } }\n";

    /// 手工写一个 zip（测守卫与识别用）。
    fn write_zip(path: &Path, entries: &[(&str, &[u8])]) {
        let mut w = zip::ZipWriter::new(fs::File::create(path).unwrap());
        for (name, data) in entries {
            w.start_file(*name, zip::write::SimpleFileOptions::default())
                .unwrap();
            w.write_all(data).unwrap();
        }
        w.finish().unwrap();
    }

    fn good_meta() -> String {
        format!(
            "[package]\nformat_version = {THEME_FORMAT_VERSION}\nkind = \"{THEME_KIND}\"\nname = \"水墨\"\n"
        )
    }

    /// 一个内容完整的包。
    fn write_good(path: &Path) {
        write_zip(
            path,
            &[
                (THEME_META_NAME, good_meta().as_bytes()),
                (THEME_ENTRY_NAME, THEME_TEXT.as_bytes()),
                (THEME_PREVIEW_NAME, b"fake-png"),
                ("assets/bg.png", b"bg-bytes"),
            ],
        );
    }

    #[test]
    fn export_then_import_round_trips() {
        let t = tempfile::tempdir().unwrap();
        let src = t.path().join("src-theme");
        fs::create_dir_all(src.join("assets")).unwrap();
        fs::write(src.join(THEME_ENTRY_NAME), THEME_TEXT).unwrap();
        fs::write(src.join("assets/bg.png"), b"bg-bytes").unwrap();
        // 主题目录里的无关文件不该进包。
        fs::write(src.join("Thumbs.db"), b"junk").unwrap();

        let pkg = t.path().join("out.wtheme");
        export_package(&src, &pkg, Some(b"preview")).unwrap();

        let preview = preview_package(&pkg).unwrap();
        assert_eq!(preview.display_name, "水墨");
        assert_eq!(preview.author, "某人");
        assert_eq!(preview.version, "1.2");
        assert_eq!(preview.asset_count, 1);
        assert!(preview.has_preview);

        let dst = t.path().join("themes").join("shuimo");
        let out = import_package(&pkg, &dst).unwrap();
        assert_eq!(out.display_name, "水墨");
        assert_eq!(
            fs::read_to_string(dst.join(THEME_ENTRY_NAME)).unwrap(),
            THEME_TEXT
        );
        assert_eq!(fs::read(dst.join("assets/bg.png")).unwrap(), b"bg-bytes");
        // package.toml / preview.png 不是主题资源，落盘只会多两个死文件。
        assert!(!dst.join(THEME_META_NAME).exists());
        assert!(!dst.join(THEME_PREVIEW_NAME).exists());
        // 导出不收无关文件。
        assert!(!dst.join("Thumbs.db").exists());
    }

    /// 覆盖导入是**整目录替换**：上一版的资源不能留成垃圾。
    #[test]
    fn reimport_replaces_whole_directory() {
        let t = tempfile::tempdir().unwrap();
        let dst = t.path().join("themes").join("x");
        fs::create_dir_all(dst.join("assets")).unwrap();
        fs::write(dst.join(THEME_ENTRY_NAME), "[meta]\nname = \"旧\"\n").unwrap();
        fs::write(dst.join("assets/old.png"), b"stale").unwrap();

        let pkg = t.path().join("p.wtheme");
        write_good(&pkg);
        import_package(&pkg, &dst).unwrap();

        assert!(dst.join("assets/bg.png").exists());
        assert!(
            !dst.join("assets/old.png").exists(),
            "上一版的资源该跟着走，否则主题目录越导越胀"
        );
    }

    /// 坏包不能把已装好的主题弄没。
    #[test]
    fn failed_import_leaves_target_untouched() {
        let t = tempfile::tempdir().unwrap();
        let dst = t.path().join("themes").join("x");
        fs::create_dir_all(&dst).unwrap();
        fs::write(dst.join(THEME_ENTRY_NAME), "[meta]\nname = \"原装\"\n").unwrap();

        let pkg = t.path().join("bad.wtheme");
        write_zip(&pkg, &[("assets/bg.png", b"only-assets")]); // 缺 theme.toml

        assert!(import_package(&pkg, &dst).is_err());
        assert_eq!(
            fs::read_to_string(dst.join(THEME_ENTRY_NAME)).unwrap(),
            "[meta]\nname = \"原装\"\n"
        );
    }

    /// 路径穿越：`../` 与反斜杠形态都不能落到主题目录外面。
    #[test]
    fn rejects_path_traversal() {
        let t = tempfile::tempdir().unwrap();
        for evil in ["../evil.png", "assets/../../evil.png", "..\\evil.png"] {
            let pkg = t.path().join("e.wtheme");
            write_zip(
                &pkg,
                &[(THEME_ENTRY_NAME, THEME_TEXT.as_bytes()), (evil, b"pwned")],
            );
            assert!(preview_package(&pkg).is_err(), "穿越条目「{evil}」该被拒");
        }
    }

    /// 不在 assets/ 下的陌生条目宁可拒绝，也不要悄悄丢掉——悄悄丢的话，作者要等主题
    /// 装进输入法、图不显示才发现自己把文件放错了位置。
    #[test]
    fn rejects_unknown_entries() {
        let t = tempfile::tempdir().unwrap();
        let pkg = t.path().join("u.wtheme");
        write_zip(
            &pkg,
            &[
                (THEME_ENTRY_NAME, THEME_TEXT.as_bytes()),
                ("bg.png", b"misplaced"),
            ],
        );
        let err = preview_package(&pkg).unwrap_err().to_string();
        assert!(err.contains("bg.png"), "报错要指出是哪个条目：{err}");
        assert!(err.contains(THEME_ASSETS_PREFIX), "还要说清该放哪儿：{err}");
    }

    #[test]
    fn rejects_missing_theme_entry() {
        let t = tempfile::tempdir().unwrap();
        let pkg = t.path().join("n.wtheme");
        write_zip(&pkg, &[(THEME_META_NAME, good_meta().as_bytes())]);
        let err = preview_package(&pkg).unwrap_err().to_string();
        assert!(err.contains(THEME_ENTRY_NAME), "{err}");
    }

    /// 扩展名只是路由提示，包内 kind 才是权威判据：方案包被改名成 .wtheme 时要说清。
    #[test]
    fn rejects_other_kinds_by_name() {
        let t = tempfile::tempdir().unwrap();
        let pkg = t.path().join("s.wtheme");
        write_zip(
            &pkg,
            &[
                (
                    THEME_META_NAME,
                    b"[package]\nformat_version = 1\nkind = \"scheme\"\n".as_slice(),
                ),
                (THEME_ENTRY_NAME, THEME_TEXT.as_bytes()),
            ],
        );
        let err = preview_package(&pkg).unwrap_err().to_string();
        assert!(err.contains("scheme"), "要说清它是什么：{err}");
    }

    /// 手工打的包可以没有 package.toml：此时按根条目推断，不该因此失败。
    #[test]
    fn accepts_hand_made_package_without_meta() {
        let t = tempfile::tempdir().unwrap();
        let pkg = t.path().join("h.wtheme");
        write_zip(
            &pkg,
            &[
                (THEME_ENTRY_NAME, THEME_TEXT.as_bytes()),
                ("assets/bg.png", b"bg"),
            ],
        );
        let p = preview_package(&pkg).unwrap();
        // package.toml 缺席 ⇒ 展示信息回退到 theme.toml 自己的 meta。
        assert_eq!(p.display_name, "水墨");
        assert_eq!(p.author, "某人");
    }

    /// 未来版本的包不能按当前规则硬解——宁可让用户去升级。
    #[test]
    fn rejects_newer_format_version() {
        let t = tempfile::tempdir().unwrap();
        let pkg = t.path().join("v.wtheme");
        let meta = format!(
            "[package]\nformat_version = {}\nkind = \"{THEME_KIND}\"\n",
            THEME_FORMAT_VERSION + 1
        );
        write_zip(
            &pkg,
            &[
                (THEME_META_NAME, meta.as_bytes()),
                (THEME_ENTRY_NAME, THEME_TEXT.as_bytes()),
            ],
        );
        let err = preview_package(&pkg).unwrap_err().to_string();
        assert!(err.contains("升级"), "要告诉用户怎么办：{err}");
    }

    #[test]
    fn rejects_too_many_entries() {
        let t = tempfile::tempdir().unwrap();
        let pkg = t.path().join("many.wtheme");
        let names: Vec<String> = (0..=MAX_THEME_ENTRIES)
            .map(|i| format!("assets/{i}.png"))
            .collect();
        let entries: Vec<(&str, &[u8])> = names
            .iter()
            .map(|n| (n.as_str(), b"x".as_slice()))
            .collect();
        write_zip(&pkg, &entries);
        let err = preview_package(&pkg).unwrap_err().to_string();
        assert!(err.contains("条目过多"), "{err}");
    }

    /// 坏主题在**落盘前**就该被拦下，而不是写完再回滚。
    #[test]
    fn rejects_invalid_theme_text_before_writing() {
        let t = tempfile::tempdir().unwrap();
        let pkg = t.path().join("b.wtheme");
        write_zip(&pkg, &[(THEME_ENTRY_NAME, "这不是 TOML {{{".as_bytes())]);
        assert!(preview_package(&pkg).is_err());

        let dst = t.path().join("themes").join("x");
        assert!(import_package(&pkg, &dst).is_err());
        assert!(!dst.exists(), "失败的导入不该留下半个主题目录");
    }

    /// 导出的条目顺序要稳定：同样的输入打出同样的包，否则每次导出都像「全变了」。
    #[test]
    fn export_entry_order_is_stable() {
        let t = tempfile::tempdir().unwrap();
        let src = t.path().join("s");
        fs::create_dir_all(src.join("assets/sub")).unwrap();
        fs::write(src.join(THEME_ENTRY_NAME), THEME_TEXT).unwrap();
        for n in ["b.png", "a.png", "sub/c.png"] {
            fs::write(src.join("assets").join(n), b"x").unwrap();
        }
        let names = |p: &Path| -> Vec<String> {
            let f = fs::File::open(p).unwrap();
            zip::ZipArchive::new(f)
                .unwrap()
                .file_names()
                .map(String::from)
                .collect()
        };
        let p1 = t.path().join("1.wtheme");
        let p2 = t.path().join("2.wtheme");
        export_package(&src, &p1, None).unwrap();
        export_package(&src, &p2, None).unwrap();
        assert_eq!(names(&p1), names(&p2));
    }
}
