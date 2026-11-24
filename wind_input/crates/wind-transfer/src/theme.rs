//! 主题包（`.wtheme`）—— 主题的 zip 分发格式。
//!
//! ```text
//! package.toml      元信息：format_version / kind / name / author / version（不落盘）
//! theme.toml        主题本体（根条目，识别主题的依据）
//! preview.png       市场预览图（可选，不落盘）
//! assets/bg.png     主题引用的图片等资源
//! ```
//!
//! ## 皮肤图片
//!
//! `[skin]` 里的图片引用（`frame.image`、`frame.srcset`、`decor.image`、`states` / `on`
//! 片段里的 `image`，或指向 `[resources]` 的键）一律写成 `assets/…`：包里只有 `assets/**`
//! 能装资源。导出时引用越界或文件不存在就报错（不自动搬运改写），导入时包里缺引用的
//! 资源同样报错。
//!
//! ## 为什么不与方案包（`.wpkg`）共用一个后缀
//!
//! 扩展名是路由判据：设置端靠它分派意图、给 ProgId 挂图标与「打开方式」，文件关联也是
//! per-扩展名的；共用后缀的话得先解压才知道是什么。
//!
//! 但扩展名只是提示，权威判据是包内 `package.toml` 的 `kind`。没有 `package.toml` 的手工包
//! 退回看根目录有没有 `theme.toml`，与 `.wpkg` 同一套取舍。
//!
//! ## 只落该落的
//!
//! `package.toml` 与 `preview.png` 按名识别、不写进主题目录（不是主题资源），同
//! [`crate::scheme`] 对 `config_patch.toml` 的处理。

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

/// 包内解压后总字节上限。
///
/// 皮肤主题带高倍率窗体图与动画装饰，几张 APNG / 动画 WebP 就是十几 MiB；64 MiB 给它们
/// 留够余量，同时把 zip 炸弹挡在把服务进程撑爆之前。
pub const MAX_THEME_UNCOMPRESSED_BYTES: u64 = 64 * 1024 * 1024;

/// 单个图片条目（`assets/**`、`preview.png`）解压后的上限。
pub const MAX_THEME_IMAGE_BYTES: u64 = 16 * 1024 * 1024;

/// `theme.toml` / `package.toml` 这类文本条目的单条上限。
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
    /// 包类型的权威判据。空 = 手工打包，退回按根条目推断。
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

/// 扫描结果：条目分类 + 元信息。不读载荷。
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

    // 规模门禁只读中央目录，排在逐条校验之前。这里的「解压后大小」是归档自己声明的，
    // 只是廉价前筛；真正的界在 `import_package` 的逐条有界读取上。
    if archive.len() > MAX_THEME_ENTRIES {
        anyhow::bail!(
            "主题包条目过多（{} 个，上限 {MAX_THEME_ENTRIES}）",
            archive.len()
        );
    }
    let mut declared: u64 = 0;
    for i in 0..archive.len() {
        let entry = archive.by_index_raw(i)?;
        let limit = entry_limit(entry.name());
        if entry.size() > limit {
            anyhow::bail!(
                "主题包条目「{}」解压后过大（声明 {} 字节，上限 {limit}）",
                entry.name(),
                entry.size()
            );
        }
        declared = declared.saturating_add(entry.size());
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
    let mut seen_folded = std::collections::HashSet::new();
    for name in &names {
        if name.ends_with('/') {
            continue;
        }
        // 判根与落盘一律用归一化值：`sub\theme.toml` 的 `contains('/')` 为 false，
        // 拿原名判根会把子目录文件当成根条目。校验同时挡住路径穿越。
        let rel = crate::bundle::validate_entry_rel(name, "")?;
        // 只差大小写的两个条目在 Windows / macOS 上落成同一个文件，后写的悄悄盖掉先写的。
        if !seen_folded.insert(rel.to_lowercase()) {
            anyhow::bail!("主题包里有只差大小写的重名条目「{rel}」");
        }
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

/// 单条目解压上限：文本条目 4 MiB，其余（图片）16 MiB。
fn entry_limit(name: &str) -> u64 {
    if name == THEME_ENTRY_NAME || name == THEME_META_NAME {
        MAX_THEME_TEXT_BYTES
    } else {
        MAX_THEME_IMAGE_BYTES
    }
}

/// 已压缩的图片格式用存储模式：再走一遍 deflate 几乎不省字节，只白费解压时间。
fn entry_options(name: &str) -> zip::write::SimpleFileOptions {
    let opts = zip::write::SimpleFileOptions::default();
    let ext = Path::new(name)
        .extension()
        .map(|e| e.to_string_lossy().to_ascii_lowercase());
    match ext.as_deref() {
        Some("png" | "webp" | "gif") => opts.compression_method(zip::CompressionMethod::Stored),
        _ => opts,
    }
}

/// `[skin]` 引用的包内文件：`(原引用, 归一化相对路径)`。
///
/// 递归收集 `skin` 表下所有 `image` 字符串（frame / srcset / decor / states / on 一网打尽），
/// 指向 `[resources]` 键的展开成该资源的路径（含 light / dark 两个变体）。`data:` 内联
/// 不占包内文件，跳过；其余必须落在 `assets/` 下，否则报错并指出是哪个引用。
fn skin_asset_refs(text: &str) -> anyhow::Result<Vec<(String, String)>> {
    let doc: toml::Table = toml::from_str(text)?;
    let Some(skin) = doc.get("skin") else {
        return Ok(Vec::new());
    };
    fn collect(v: &toml::Value, out: &mut Vec<String>) {
        match v {
            toml::Value::Table(t) => {
                for (k, v) in t {
                    match v {
                        toml::Value::String(s) if k == "image" => out.push(s.clone()),
                        _ => collect(v, out),
                    }
                }
            }
            toml::Value::Array(a) => a.iter().for_each(|v| collect(v, out)),
            _ => {}
        }
    }
    let mut images = Vec::new();
    collect(skin, &mut images);

    let resources = doc.get("resources").and_then(toml::Value::as_table);
    let mut paths: Vec<String> = Vec::new();
    for image in images {
        match resources.and_then(|r| r.get(&image)) {
            Some(toml::Value::String(p)) => paths.push(p.clone()),
            Some(toml::Value::Table(t)) => paths.extend(
                ["light", "dark"]
                    .iter()
                    .filter_map(|k| t.get(*k).and_then(toml::Value::as_str))
                    .map(String::from),
            ),
            _ => paths.push(image),
        }
    }
    paths.sort();
    paths.dedup();

    let mut out = Vec::new();
    for p in paths {
        if p.is_empty() || p.starts_with("data:") {
            continue;
        }
        // 去掉 `.` 段（`./assets/x.png` 与 `assets/x.png` 是同一个文件）；尾随点 / 空格、
        // 保留名由 `validate_entry_rel` 拒绝——Windows 会把它们解析成另一个名字。
        let cleaned: Vec<&str> = p.split(['/', '\\']).filter(|s| *s != ".").collect();
        let rel = crate::bundle::validate_entry_rel(&cleaned.join("/"), "")
            .ok()
            .filter(|r| r.len() > THEME_ASSETS_PREFIX.len() && r.starts_with(THEME_ASSETS_PREFIX))
            .ok_or_else(|| {
                // 不像路径的裸名（没有分隔符也没有扩展名）是资源键：多半是子主题引用了
                // base 的 `[resources]`，而主题包不带父主题。
                if !p.contains(['/', '\\', '.']) {
                    anyhow::anyhow!(
                        "皮肤引用的资源键「{p}」未在本主题定义（主题包不带 base 父主题的资源，请在本主题的 [resources] 里定义并把图放进 {THEME_ASSETS_PREFIX}）"
                    )
                } else {
                    anyhow::anyhow!(
                        "皮肤引用「{p}」不在 {THEME_ASSETS_PREFIX} 下（皮肤图片要放进主题的 {THEME_ASSETS_PREFIX} 目录，引用写成 {THEME_ASSETS_PREFIX}文件名）"
                    )
                }
            })?;
        out.push((p, rel));
    }
    Ok(out)
}

/// 包里要有 `[skin]` 引用的全部资源。
fn check_skin_refs_in_package(text: &str, s: &Scanned) -> anyhow::Result<()> {
    for (orig, rel) in skin_asset_refs(text)? {
        if !s.payload.iter().any(|(_, r)| *r == rel) {
            anyhow::bail!("主题包缺少皮肤引用的资源「{orig}」");
        }
    }
    Ok(())
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
    check_skin_refs_in_package(&text, &s)?;
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
/// 整目录替换而不是逐文件覆盖（资源可能改名，主题目录按 id 独占）。先写 `.tmp` 兄弟目录，
/// 成功后才动目标，中途失败目标原封不动。
///
/// `verify` 在新目录已就位、旧目录尚未删除时调用，失败则回滚到旧内容。有些校验（如 `base`
/// 继承链）要看得到真实目录才做得了，放在这里回滚逻辑才只有一份。
pub fn import_package<V>(
    package: &Path,
    target_dir: &Path,
    verify: V,
) -> anyhow::Result<ThemeImportOutcome>
where
    V: FnOnce(&Path) -> anyhow::Result<()>,
{
    let s = scan(package)?;
    let text = read_theme_text(package, &s.theme_entry)?;
    wind_theme::validate_text(&text)?;
    check_skin_refs_in_package(&text, &s)?;
    let (tname, _, _) = meta_from_theme_text(&text);
    let display_name = if s.meta.package.name.is_empty() {
        tname
    } else {
        s.meta.package.name.clone()
    };

    // 读取阶段：全部载荷入内存，任何坏条目在写盘前失败。
    //
    // 逐条有界读取，预算是整包共享的余额：`scan` 的前筛信的是归档自己声明的大小。
    let mut remaining = MAX_THEME_UNCOMPRESSED_BYTES;
    let mut staged: Vec<(String, Vec<u8>)> = Vec::new();
    for (name, rel) in &s.payload {
        let limit = remaining.min(entry_limit(rel));
        let bytes = crate::bundle::extract_entry_limited(package, name, limit)?;
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
    // 就位之后才校验：`base` 继承链这类判断要在真实目录里才做得了。
    if let Err(e) = verify(target_dir) {
        let _ = std::fs::remove_dir_all(target_dir);
        if had_old {
            let _ = std::fs::rename(&old_dir, target_dir);
        }
        return Err(e);
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
///
/// `assets/**` 的收集不跟随符号链接（`assets` 本身是链接也算）：不会因成环无限递归，也不会
/// 把主题目录外的文件打进包。皮肤引用了被跳过的链接时按「文件不存在」报错。
///
/// 写包前按导入侧的全部规则过一遍，不做出导入时必然被拒的包。
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
    let meta_text = toml::to_string_pretty(&meta)?;

    // 资源条目：(包内名, 源文件)。
    let mut assets: Vec<(String, PathBuf)> = Vec::new();
    let assets_dir = theme_dir.join("assets");
    if std::fs::symlink_metadata(&assets_dir).is_ok_and(|m| m.is_dir()) {
        let mut files: Vec<PathBuf> = Vec::new();
        collect_files(&assets_dir, &mut files)?;
        // 条目顺序稳定：同样的输入要打出同样的包，否则每次导出都是「全变了」。
        files.sort();
        for path in files {
            let rel = path
                .strip_prefix(&assets_dir)
                .map_err(|_| anyhow::anyhow!("资源路径越界：{}", path.display()))?;
            let entry = format!(
                "{THEME_ASSETS_PREFIX}{}",
                rel.to_string_lossy().replace('\\', "/")
            );
            crate::bundle::validate_entry_rel(&entry, "").map_err(|e| {
                anyhow::anyhow!("资源文件名不能进主题包：{}（{e}）", path.display())
            })?;
            assets.push((entry, path));
        }
    }

    // 与导入侧同一套门禁，按导入时的口径算。
    let entry_count = 2 + usize::from(preview_png.is_some()) + assets.len();
    if entry_count > MAX_THEME_ENTRIES {
        anyhow::bail!("主题资源过多（打包后 {entry_count} 个条目，上限 {MAX_THEME_ENTRIES}）");
    }
    let mut total = (meta_text.len() + text.len()) as u64;
    if text.len() as u64 > MAX_THEME_TEXT_BYTES {
        anyhow::bail!(
            "{} 过大（{} 字节，上限 {MAX_THEME_TEXT_BYTES}）",
            theme_path.display(),
            text.len()
        );
    }
    if let Some(png) = preview_png {
        if png.len() as u64 > MAX_THEME_IMAGE_BYTES {
            anyhow::bail!(
                "预览图过大（{} 字节，上限 {MAX_THEME_IMAGE_BYTES}）",
                png.len()
            );
        }
        total += png.len() as u64;
    }
    let mut seen_folded = std::collections::HashMap::new();
    for (entry, path) in &assets {
        if let Some(prev) = seen_folded.insert(entry.to_lowercase(), entry) {
            anyhow::bail!(
                "资源「{prev}」与「{entry}」只差大小写，在 Windows / macOS 上会落成同一个文件"
            );
        }
        let len = std::fs::metadata(path)?.len();
        if len > MAX_THEME_IMAGE_BYTES {
            anyhow::bail!(
                "资源文件过大：{}（{len} 字节，单个上限 {MAX_THEME_IMAGE_BYTES}）",
                path.display()
            );
        }
        total += len;
    }
    if total > MAX_THEME_UNCOMPRESSED_BYTES {
        anyhow::bail!("主题资源总量过大（{total} 字节，上限 {MAX_THEME_UNCOMPRESSED_BYTES}）");
    }

    // 皮肤引用按实际收集到的条目名精确比对，不用 `is_file()`：大小写不敏感的文件系统上
    // `assets/Frame.png` 能打开 `frame.png`，导出过了，导入却按包内名严格比对而失败。
    for (orig, rel) in skin_asset_refs(&text)? {
        if assets.iter().any(|(e, _)| *e == rel) {
            continue;
        }
        let folded = rel.to_lowercase();
        if let Some((e, _)) = assets.iter().find(|(e, _)| e.to_lowercase() == folded) {
            anyhow::bail!("皮肤引用「{orig}」与资源文件「{e}」大小写不一致");
        }
        anyhow::bail!(
            "皮肤引用「{orig}」指向的文件不存在：{}",
            theme_dir.join(&rel).display()
        );
    }

    if let Some(parent) = out_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let file = std::fs::File::create(out_path)?;
    let mut w = zip::ZipWriter::new(file);
    let opts = zip::write::SimpleFileOptions::default();

    w.start_file(THEME_META_NAME, opts)?;
    w.write_all(meta_text.as_bytes())?;
    w.start_file(THEME_ENTRY_NAME, opts)?;
    w.write_all(text.as_bytes())?;
    if let Some(png) = preview_png {
        w.start_file(THEME_PREVIEW_NAME, entry_options(THEME_PREVIEW_NAME))?;
        w.write_all(png)?;
    }
    for (entry, path) in assets {
        let opts = entry_options(&entry);
        w.start_file(entry, opts)?;
        w.write_all(&std::fs::read(&path)?)?;
    }
    w.finish()?;
    Ok(())
}

/// 递归收集目录下的普通文件。`file_type()` 不跟随符号链接：链接一律跳过（不进包、
/// 不递归），所以不会因链接成环而无限递归，也不会把链接指向的外部文件收进来。
fn collect_files(dir: &Path, out: &mut Vec<PathBuf>) -> anyhow::Result<()> {
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let ty = entry.file_type()?;
        if ty.is_dir() {
            collect_files(&entry.path(), out)?;
        } else if ty.is_file() {
            out.push(entry.path());
        }
    }
    Ok(())
}

#[cfg(test)]
#[path = "theme_tests.rs"]
mod tests;
