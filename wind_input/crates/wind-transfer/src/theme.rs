//! 主题包（`.wtheme`）—— 主题的 zip 分发格式。
//!
//! ```text
//! package.toml      元信息：format_version / kind / name / author / version（**不落盘**）
//! theme.toml        主题本体（根条目，识别主题的依据）
//! preview.png       市场预览图（可选，**不落盘**）
//! assets/bg.png     主题引用的图片等资源
//! ```
//!
//! ## 皮肤图片
//!
//! `[skin]` 里的图片引用（`frame.image`、`frame.srcset`、`decor.image`、`states` / `on`
//! 片段里的 `image`，或指向 `[resources]` 的键）一律写成 `assets/…`：包里只有 `assets/**`
//! 能装资源。导出时引用越界或文件不存在就报错（不自动搬运改写），导入时包里缺引用的
//! 资源同样报错——缺图的皮肤装上去只会画出一个残缺的窗体。
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
/// 皮肤主题带高倍率窗体图与动画装饰，几张 APNG / 动画 WebP 就是十几 MiB；64 MiB 给它们
/// 留够余量，同时把 zip 炸弹挡在把服务进程撑爆之前。
pub const MAX_THEME_UNCOMPRESSED_BYTES: u64 = 64 * 1024 * 1024;

/// 单个图片条目（`assets/**`、`preview.png`）解压后的上限。
pub const MAX_THEME_IMAGE_BYTES: u64 = 16 * 1024 * 1024;

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
/// **整目录替换**而不是逐文件覆盖：资源可能改名，逐文件写会把上一版的图留成垃圾，
/// 而主题目录是按 id 独占的，没有「用户往里手工放了东西」这种用法。
/// 先写 `.tmp` 兄弟目录、成功后才动目标，中途失败目标原封不动。
///
/// `verify` 在**新目录已就位、旧目录尚未删除**的那一刻被调用，失败则整体回滚到旧内容。
/// 它存在是因为有些校验只有看得到真实目录才做得了——比如主题的 `base` 继承链要在
/// 主题目录群里定位被继承的那个。把这一步交给调用方，回滚逻辑就只有这里一份；
/// 让调用方自己在事后校验的话，它手上已经没有旧内容可回滚了。
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
    // 逐条**有界**读取，且预算是整包共享的余额——`scan` 那道前筛读的是归档自己声明的
    // 大小，撒谎的包骗得过它（声明 1 KB、解压 10 GB）。
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
/// `assets/**` 的收集**不跟随符号链接**：链接（指向文件或目录都算，`assets` 本身是链接
/// 也算）既不进包也不递归，因此链接成环不会无限递归，指向主题目录外（比如家目录）的
/// 文件也不会被打进可分发的包里。皮肤引用了被跳过的链接时按「文件不存在」报错。
///
/// 写包前先按导入侧的规则全部过一遍（条目名、只差大小写的重名、条目数、单条 / 总量
/// 上限、皮肤引用），任何一条不过就报错并指出是哪个文件，不做出导入时必然被拒的包。
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

    // 皮肤引用按**实际收集到的条目名**精确比对，不用 `is_file()`：大小写不敏感的文件系统上
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
mod tests {
    use super::*;
    use std::fs;

    const THEME_TEXT: &str = "[meta]\nname = \"水墨\"\nauthor = \"某人\"\nversion = \"1.2\"\n\n[window]\nbackground = { image = { ref = \"assets/bg.png\" } }\n";

    /// 手工写一个 zip（测守卫与识别用）。
    fn write_zip(path: &Path, entries: &[(&str, &[u8])]) {
        let mut w = zip::ZipWriter::new(fs::File::create(path).unwrap());
        for (name, data) in entries {
            // 图片条目走存储模式，大体积用例不必在 debug 下跑 deflate。
            w.start_file(*name, entry_options(name)).unwrap();
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
        let out = import_package(&pkg, &dst, |_| Ok(())).unwrap();
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
        import_package(&pkg, &dst, |_| Ok(())).unwrap();

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

        assert!(import_package(&pkg, &dst, |_| Ok(())).is_err());
        assert_eq!(
            fs::read_to_string(dst.join(THEME_ENTRY_NAME)).unwrap(),
            "[meta]\nname = \"原装\"\n"
        );
    }

    /// 就位后的校验失败要整体回滚：装不上的新版不能把用户原来的主题顶掉。
    ///
    /// 这一步只能在包已经就位、旧目录还没删的那个窗口里做，所以回滚也只能在这里做
    /// ——调用方拿到失败时，手上已经没有旧内容了。
    #[test]
    fn verify_failure_rolls_back_to_previous_theme() {
        let t = tempfile::tempdir().unwrap();
        let dst = t.path().join("themes").join("x");
        fs::create_dir_all(dst.join("assets")).unwrap();
        fs::write(dst.join(THEME_ENTRY_NAME), "[meta]\nname = \"原装\"\n").unwrap();
        fs::write(dst.join("assets/keep.png"), b"mine").unwrap();

        let pkg = t.path().join("p.wtheme");
        write_good(&pkg);
        let err = import_package(&pkg, &dst, |_| anyhow::bail!("base 引用的基础主题不存在"))
            .unwrap_err()
            .to_string();

        assert!(err.contains("base"), "校验失败的原因要透传：{err}");
        assert_eq!(
            fs::read_to_string(dst.join(THEME_ENTRY_NAME)).unwrap(),
            "[meta]\nname = \"原装\"\n"
        );
        assert!(dst.join("assets/keep.png").exists(), "旧资源该跟着回来");
        assert!(!dst.join("assets/bg.png").exists(), "新版的资源不该残留");
    }

    /// 新装时校验失败：不留半个主题目录，否则主题列表里会多出一个打不开的条目。
    #[test]
    fn verify_failure_on_fresh_install_leaves_nothing() {
        let t = tempfile::tempdir().unwrap();
        let dst = t.path().join("themes").join("brand-new");
        let pkg = t.path().join("p.wtheme");
        write_good(&pkg);

        assert!(import_package(&pkg, &dst, |_| anyhow::bail!("装不上")).is_err());
        assert!(!dst.exists());
    }

    /// 校验看到的是**就位后的真实目录**——依赖链那类判断只有在这个位置才做得了。
    #[test]
    fn verify_sees_the_placed_directory() {
        let t = tempfile::tempdir().unwrap();
        let dst = t.path().join("themes").join("x");
        let pkg = t.path().join("p.wtheme");
        write_good(&pkg);

        let mut seen = None;
        import_package(&pkg, &dst, |dir| {
            seen = Some(dir.join(THEME_ENTRY_NAME).exists() && dir.join("assets/bg.png").exists());
            Ok(())
        })
        .unwrap();
        assert_eq!(seen, Some(true), "校验时主题应当已经在目标位置了");
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
        assert!(import_package(&pkg, &dst, |_| Ok(())).is_err());
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

    // ── 皮肤 ──

    /// 程序生成的单帧 PNG（纯色 RGBA）。
    fn png_bytes(w: u32, h: u32, rgba: [u8; 4]) -> Vec<u8> {
        apng_bytes(w, h, &[rgba])
    }

    /// 程序生成的 APNG：每帧一种纯色；只有一帧时就是普通 PNG。
    fn apng_bytes(w: u32, h: u32, frames: &[[u8; 4]]) -> Vec<u8> {
        let mut out = Vec::new();
        {
            let mut enc = png::Encoder::new(&mut out, w, h);
            enc.set_color(png::ColorType::Rgba);
            enc.set_depth(png::BitDepth::Eight);
            if frames.len() > 1 {
                enc.set_animated(frames.len() as u32, 0).unwrap();
            }
            let mut wr = enc.write_header().unwrap();
            for c in frames {
                let px: Vec<u8> = c.repeat((w * h) as usize);
                wr.write_image_data(&px).unwrap();
            }
            wr.finish().unwrap();
        }
        out
    }

    /// 帧数（APNG 的 acTL；普通 PNG 为 1）。
    fn png_frame_count(bytes: &[u8]) -> u32 {
        let r = png::Decoder::new(std::io::Cursor::new(bytes))
            .read_info()
            .unwrap();
        r.info().animation_control.map_or(1, |a| a.num_frames)
    }

    const SKIN_THEME: &str = r#"[meta]
name = "皮肤"

[resources]
frame_main = { light = "assets/frame.png", dark = "assets/frame_dark.png" }

[skin.horizontal.frame]
image = "frame_main"
srcset = [{ image = "assets/frame@2x.png", density = 2 }]

[[skin.horizontal.decor]]
image = "assets/pet.png"
anchor = "frame.top_right"

[skin.horizontal.decor.states]
idle = { frames = "0-1", loop = true }
alt = { image = "assets/alt.png" }

[skin.horizontal.decor.on]
commit = { play = { image = "assets/jump.png" }, then = "idle" }
"#;

    /// 皮肤主题目录：窗体走 resources（含暗色变体）+ srcset，装饰是 APNG，
    /// states / on 各引用另一张图。
    fn write_skin_theme(dir: &Path) {
        fs::create_dir_all(dir.join("assets")).unwrap();
        fs::write(dir.join(THEME_ENTRY_NAME), SKIN_THEME).unwrap();
        for (n, c) in [
            ("frame.png", [255, 0, 0, 255]),
            ("frame_dark.png", [0, 0, 0, 255]),
            ("frame@2x.png", [255, 0, 0, 255]),
            ("alt.png", [0, 255, 0, 255]),
            ("jump.png", [0, 0, 255, 255]),
        ] {
            fs::write(dir.join("assets").join(n), png_bytes(4, 4, c)).unwrap();
        }
        let pet = apng_bytes(4, 4, &[[255, 255, 0, 255], [0, 255, 255, 255]]);
        fs::write(dir.join("assets/pet.png"), pet).unwrap();
    }

    /// 导出 → 导入 → 按实机的解析规则（resources 键优先，否则主题目录 join 引用）
    /// 找到每一张皮肤图，且 APNG 帧数原样保留。
    #[test]
    fn skin_theme_round_trips_and_resolves() {
        let t = tempfile::tempdir().unwrap();
        let src = t.path().join("src");
        write_skin_theme(&src);
        let pkg = t.path().join("skin.wtheme");
        export_package(&src, &pkg, None).unwrap();
        assert_eq!(preview_package(&pkg).unwrap().asset_count, 6);

        let themes = t.path().join("themes");
        import_package(&pkg, &themes.join("skin"), |_| Ok(())).unwrap();

        for dark in [false, true] {
            let r = wind_theme::resolve::load_resolved_dirs(
                std::slice::from_ref(&themes),
                "skin",
                dark,
            )
            .unwrap();
            // 同 wind-scene `theme_assets::asset_path` 的查找顺序。
            let locate = |reference: &str| -> PathBuf {
                r.resources
                    .get(reference)
                    .map(PathBuf::from)
                    .unwrap_or_else(|| r.asset_dirs[0].join(reference))
            };
            let h = r.skin.as_ref().unwrap().horizontal.as_ref().unwrap();
            let frame = locate(&h.frame.image);
            assert!(frame.ends_with(if dark { "frame_dark.png" } else { "frame.png" }));
            assert!(frame.is_file());
            assert!(locate(&h.frame.srcset[0].0).is_file());

            let decor = &h.decor[0];
            let pet = fs::read(locate(&decor.image)).unwrap();
            assert_eq!(png_frame_count(&pet), 2, "动画装饰要原样带过去");
            let alt = decor.anim.states.iter().find(|(n, _)| n == "alt").unwrap();
            let wind_theme::skin::DecorClip::Image(alt) = &alt.1 else {
                panic!("alt 应是另一张图");
            };
            assert!(locate(alt).is_file());
            let Some((_, wind_theme::skin::DecorAction::Play { clip, .. })) = decor.anim.on.first()
            else {
                panic!("commit 应是一次性片段");
            };
            let wind_theme::skin::DecorClip::Image(jump) = clip else {
                panic!("commit 片段应是另一张图");
            };
            assert!(locate(jump).is_file());
        }
    }

    /// 引用越出 assets/：导出报错并指出是哪个引用，不自动搬运。
    #[test]
    fn export_rejects_skin_ref_outside_assets() {
        let t = tempfile::tempdir().unwrap();
        for bad in ["frame.png", "../frame.png", "/abs/frame.png"] {
            let src = t.path().join("src");
            write_skin_theme(&src);
            fs::write(src.join("frame.png"), png_bytes(2, 2, [1, 2, 3, 255])).unwrap();
            let text = SKIN_THEME.replace("assets/frame@2x.png", bad);
            fs::write(src.join(THEME_ENTRY_NAME), text).unwrap();
            let err = export_package(&src, &t.path().join("o.wtheme"), None)
                .unwrap_err()
                .to_string();
            assert!(err.contains(bad), "要指出是哪个引用：{err}");
            assert!(err.contains(THEME_ASSETS_PREFIX), "要说清该放哪儿：{err}");
        }
    }

    /// resources 键指向的路径同样要在 assets/ 下。
    #[test]
    fn export_rejects_resource_path_outside_assets() {
        let t = tempfile::tempdir().unwrap();
        let src = t.path().join("src");
        write_skin_theme(&src);
        let text = SKIN_THEME.replace("\"assets/frame_dark.png\"", "\"frame_dark.png\"");
        fs::write(src.join(THEME_ENTRY_NAME), text).unwrap();
        let err = export_package(&src, &t.path().join("o.wtheme"), None)
            .unwrap_err()
            .to_string();
        assert!(err.contains("frame_dark.png"), "{err}");
    }

    #[test]
    fn export_rejects_missing_skin_asset() {
        let t = tempfile::tempdir().unwrap();
        let src = t.path().join("src");
        write_skin_theme(&src);
        fs::remove_file(src.join("assets/jump.png")).unwrap();
        let err = export_package(&src, &t.path().join("o.wtheme"), None)
            .unwrap_err()
            .to_string();
        assert!(err.contains("assets/jump.png"), "{err}");
    }

    /// 包里缺皮肤引用的资源：预览就拦下，导入也不动已装的主题。
    #[test]
    fn import_rejects_package_missing_skin_asset() {
        let t = tempfile::tempdir().unwrap();
        let pkg = t.path().join("m.wtheme");
        let text = "[meta]\nname = \"缺图\"\n[skin.horizontal.frame]\nimage = \"assets/frame.png\"\n[[skin.horizontal.decor]]\nimage = \"assets/pet.png\"\n";
        let frame = png_bytes(2, 2, [1, 2, 3, 255]);
        write_zip(
            &pkg,
            &[
                (THEME_ENTRY_NAME, text.as_bytes()),
                ("assets/frame.png", &frame),
            ],
        );
        let err = preview_package(&pkg).unwrap_err().to_string();
        assert!(err.contains("assets/pet.png"), "{err}");

        let dst = t.path().join("themes").join("x");
        fs::create_dir_all(&dst).unwrap();
        fs::write(dst.join(THEME_ENTRY_NAME), "[meta]\nname = \"原装\"\n").unwrap();
        let mut verified = false;
        let err = import_package(&pkg, &dst, |_| {
            verified = true;
            Ok(())
        })
        .unwrap_err()
        .to_string();
        assert!(err.contains("assets/pet.png"), "{err}");
        assert!(!verified, "缺图在落盘前就该拦下");
        assert_eq!(
            fs::read_to_string(dst.join(THEME_ENTRY_NAME)).unwrap(),
            "[meta]\nname = \"原装\"\n"
        );
    }

    /// data: 内联图不占包内文件，不算缺失。
    #[test]
    fn skin_data_uri_needs_no_asset() {
        let t = tempfile::tempdir().unwrap();
        let pkg = t.path().join("d.wtheme");
        let text = "[meta]\nname = \"内联\"\n[skin.horizontal.frame]\nimage = \"data:image/svg+xml;base64,PHN2Zy8+\"\n";
        write_zip(&pkg, &[(THEME_ENTRY_NAME, text.as_bytes())]);
        preview_package(&pkg).unwrap();
    }

    const MIB: usize = 1024 * 1024;

    #[test]
    fn rejects_image_over_single_entry_limit() {
        let t = tempfile::tempdir().unwrap();
        let pkg = t.path().join("big.wtheme");
        let big = vec![0u8; MAX_THEME_IMAGE_BYTES as usize + 1];
        write_zip(
            &pkg,
            &[
                (THEME_ENTRY_NAME, THEME_TEXT.as_bytes()),
                ("assets/big.png", &big),
            ],
        );
        let err = preview_package(&pkg).unwrap_err().to_string();
        assert!(err.contains("assets/big.png"), "要指出是哪张图：{err}");
    }

    /// 旧上限 32 MiB 装不下、新上限 64 MiB 内的皮肤包能导入。
    #[test]
    fn accepts_total_between_old_and_new_limit() {
        let t = tempfile::tempdir().unwrap();
        let pkg = t.path().join("ok.wtheme");
        let img = vec![0u8; 14 * MIB];
        write_zip(
            &pkg,
            &[
                (THEME_ENTRY_NAME, THEME_TEXT.as_bytes()),
                ("assets/bg.png", &img),
                ("assets/b.png", &img),
                ("assets/c.png", &img),
            ],
        );
        let dst = t.path().join("themes").join("x");
        import_package(&pkg, &dst, |_| Ok(())).unwrap();
        assert_eq!(
            fs::metadata(dst.join("assets/c.png")).unwrap().len(),
            img.len() as u64
        );
    }

    #[test]
    fn rejects_total_over_limit() {
        let t = tempfile::tempdir().unwrap();
        let pkg = t.path().join("huge.wtheme");
        let img = vec![0u8; 14 * MIB];
        let names: Vec<String> = (0..5).map(|i| format!("assets/{i}.png")).collect();
        let mut entries: Vec<(&str, &[u8])> = vec![(THEME_ENTRY_NAME, THEME_TEXT.as_bytes())];
        entries.extend(names.iter().map(|n| (n.as_str(), img.as_slice())));
        write_zip(&pkg, &entries);
        let err = preview_package(&pkg).unwrap_err().to_string();
        assert!(err.contains("过大"), "{err}");
    }

    /// PNG / WebP / GIF 用存储模式；文本与其它资源照旧 deflate。
    #[test]
    fn export_stores_compressed_images() {
        let t = tempfile::tempdir().unwrap();
        let src = t.path().join("s");
        fs::create_dir_all(src.join("assets")).unwrap();
        fs::write(src.join(THEME_ENTRY_NAME), THEME_TEXT).unwrap();
        for n in ["bg.png", "a.WEBP", "b.gif", "c.svg"] {
            fs::write(src.join("assets").join(n), vec![b'x'; 4096]).unwrap();
        }
        let pkg = t.path().join("o.wtheme");
        export_package(&src, &pkg, Some(&png_bytes(2, 2, [0, 0, 0, 255]))).unwrap();

        let mut a = zip::ZipArchive::new(fs::File::open(&pkg).unwrap()).unwrap();
        let method =
            |a: &mut zip::ZipArchive<fs::File>, n: &str| a.by_name(n).unwrap().compression();
        use zip::CompressionMethod::{Deflated, Stored};
        for n in [
            "assets/bg.png",
            "assets/a.WEBP",
            "assets/b.gif",
            THEME_PREVIEW_NAME,
        ] {
            assert_eq!(method(&mut a, n), Stored, "{n}");
        }
        for n in [THEME_ENTRY_NAME, THEME_META_NAME, "assets/c.svg"] {
            assert_eq!(method(&mut a, n), Deflated, "{n}");
        }
    }

    /// 导出时引用与收集到的条目名精确比对：大小写不敏感的文件系统上 `is_file()` 会放行
    /// `assets/Frame.png` → `frame.png`，做出来的包自己却导不回来。
    #[test]
    fn export_rejects_skin_ref_case_mismatch() {
        let t = tempfile::tempdir().unwrap();
        let src = t.path().join("src");
        write_skin_theme(&src);
        let text = SKIN_THEME.replace("assets/alt.png", "assets/Alt.png");
        fs::write(src.join(THEME_ENTRY_NAME), text).unwrap();
        let err = export_package(&src, &t.path().join("o.wtheme"), None)
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("assets/Alt.png") && err.contains("大小写"),
            "{err}"
        );
    }

    /// `./` 段规范化后按同一个文件处理，导出、导入都认。
    #[test]
    fn skin_ref_with_dot_segment_round_trips() {
        let t = tempfile::tempdir().unwrap();
        let src = t.path().join("src");
        write_skin_theme(&src);
        let text = SKIN_THEME.replace("\"assets/pet.png\"", "\"./assets/./pet.png\"");
        fs::write(src.join(THEME_ENTRY_NAME), text).unwrap();
        let pkg = t.path().join("o.wtheme");
        export_package(&src, &pkg, None).unwrap();
        import_package(&pkg, &t.path().join("themes/x"), |_| Ok(())).unwrap();
    }

    /// 尾随点 / 空格、保留名：Windows 会解析成另一个文件，直接拒。
    #[test]
    fn export_rejects_skin_ref_windows_would_rename() {
        let t = tempfile::tempdir().unwrap();
        for bad in ["assets/pet.png.", "assets/pet.png ", "assets/NUL.png"] {
            let src = t.path().join("src");
            write_skin_theme(&src);
            let text = SKIN_THEME.replace("assets/pet.png", bad);
            fs::write(src.join(THEME_ENTRY_NAME), text).unwrap();
            let err = export_package(&src, &t.path().join("o.wtheme"), None)
                .unwrap_err()
                .to_string();
            assert!(err.contains(bad), "{bad:?}：{err}");
        }
    }

    /// 子主题引用父主题 `[resources]` 的键：说清是资源键没定义，不说「不在 assets/ 下」。
    #[test]
    fn export_reports_undefined_resource_key() {
        let t = tempfile::tempdir().unwrap();
        let src = t.path().join("src");
        fs::create_dir_all(&src).unwrap();
        fs::write(
            src.join(THEME_ENTRY_NAME),
            "base = \"parent\"\n[meta]\nname = \"子\"\n[skin.horizontal.frame]\nimage = \"frame_main\"\n",
        )
        .unwrap();
        let err = export_package(&src, &t.path().join("o.wtheme"), None)
            .unwrap_err()
            .to_string();
        assert!(err.contains("资源键「frame_main」未在本主题定义"), "{err}");
    }

    #[test]
    fn export_rejects_oversized_image_before_writing() {
        let t = tempfile::tempdir().unwrap();
        let src = t.path().join("s");
        fs::create_dir_all(src.join("assets")).unwrap();
        fs::write(src.join(THEME_ENTRY_NAME), THEME_TEXT).unwrap();
        fs::write(
            src.join("assets/big.png"),
            vec![0u8; MAX_THEME_IMAGE_BYTES as usize + 1],
        )
        .unwrap();
        let out = t.path().join("o.wtheme");
        let err = export_package(&src, &out, None).unwrap_err().to_string();
        assert!(err.contains("big.png"), "要指出是哪个文件：{err}");
        assert!(!out.exists(), "不该留下导入时必然被拒的包");
    }

    #[test]
    fn export_rejects_total_over_limit() {
        let t = tempfile::tempdir().unwrap();
        let src = t.path().join("s");
        fs::create_dir_all(src.join("assets")).unwrap();
        fs::write(src.join(THEME_ENTRY_NAME), THEME_TEXT).unwrap();
        let img = vec![0u8; 14 * MIB];
        for i in 0..5 {
            fs::write(src.join(format!("assets/{i}.png")), &img).unwrap();
        }
        let err = export_package(&src, &t.path().join("o.wtheme"), None)
            .unwrap_err()
            .to_string();
        assert!(err.contains("总量过大"), "{err}");
    }

    #[test]
    fn export_rejects_too_many_entries() {
        let t = tempfile::tempdir().unwrap();
        let src = t.path().join("s");
        fs::create_dir_all(src.join("assets")).unwrap();
        fs::write(src.join(THEME_ENTRY_NAME), THEME_TEXT).unwrap();
        // package.toml + theme.toml + 199 张 = 201。
        for i in 0..MAX_THEME_ENTRIES - 1 {
            fs::write(src.join(format!("assets/{i}.png")), b"x").unwrap();
        }
        let err = export_package(&src, &t.path().join("o.wtheme"), None)
            .unwrap_err()
            .to_string();
        assert!(err.contains("过多"), "{err}");
    }

    /// 只差大小写的资源：大小写敏感的文件系统上导得出，装到 Windows / macOS 却互相覆盖。
    #[test]
    fn export_rejects_case_only_duplicate_assets() {
        let t = tempfile::tempdir().unwrap();
        let src = t.path().join("s");
        fs::create_dir_all(src.join("assets")).unwrap();
        fs::write(src.join(THEME_ENTRY_NAME), THEME_TEXT).unwrap();
        fs::write(src.join("assets/Bg.png"), b"a").unwrap();
        if fs::write(src.join("assets/bg.png"), b"b").is_err()
            || fs::read_dir(src.join("assets")).unwrap().count() < 2
        {
            return; // 大小写不敏感的文件系统上造不出这种目录。
        }
        let err = export_package(&src, &t.path().join("o.wtheme"), None)
            .unwrap_err()
            .to_string();
        assert!(err.contains("只差大小写"), "{err}");
    }

    #[test]
    fn import_rejects_case_only_duplicate_entries() {
        let t = tempfile::tempdir().unwrap();
        let pkg = t.path().join("c.wtheme");
        write_zip(
            &pkg,
            &[
                (THEME_ENTRY_NAME, THEME_TEXT.as_bytes()),
                ("assets/bg.png", b"a"),
                ("assets/BG.png", b"b"),
            ],
        );
        let err = preview_package(&pkg).unwrap_err().to_string();
        assert!(err.contains("只差大小写"), "{err}");
    }

    #[test]
    fn import_rejects_windows_reserved_names() {
        let t = tempfile::tempdir().unwrap();
        for bad in ["assets/NUL.png", "assets/con", "assets/x.png."] {
            let pkg = t.path().join("r.wtheme");
            write_zip(
                &pkg,
                &[(THEME_ENTRY_NAME, THEME_TEXT.as_bytes()), (bad, b"x")],
            );
            assert!(preview_package(&pkg).is_err(), "{bad} 该被拒");
        }
    }

    /// 撒谎的条目：中央目录与本地头都声明 1 KB，实际解压 17 MiB。规模前筛骗得过，
    /// 真正读取时的有界读取必须拦下。
    #[test]
    fn import_bounds_actual_size_not_declared_size() {
        let t = tempfile::tempdir().unwrap();
        let pkg = t.path().join("bomb.wtheme");
        let name = "assets/bomb.bin";
        {
            let mut w = zip::ZipWriter::new(fs::File::create(&pkg).unwrap());
            let opts = zip::write::SimpleFileOptions::default();
            w.start_file(THEME_ENTRY_NAME, opts).unwrap();
            w.write_all(THEME_TEXT.as_bytes()).unwrap();
            w.start_file(name, opts).unwrap();
            w.write_all(&vec![0u8; 17 * MIB]).unwrap();
            w.finish().unwrap();
        }
        // 改写 uncompressed size：本地头 sig 0x04034b50 偏移 22、名字在 30；
        // 中央目录 sig 0x02014b50 偏移 24、名字在 46。
        let mut b = fs::read(&pkg).unwrap();
        let mut patched = 0;
        for i in 0..b.len().saturating_sub(46 + name.len()) {
            let (size_at, name_at) = match &b[i..i + 4] {
                [0x50, 0x4b, 0x03, 0x04] => (22, 30),
                [0x50, 0x4b, 0x01, 0x02] => (24, 46),
                _ => continue,
            };
            if &b[i + name_at..i + name_at + name.len()] == name.as_bytes() {
                b[i + size_at..i + size_at + 4].copy_from_slice(&1024u32.to_le_bytes());
                patched += 1;
            }
        }
        assert_eq!(patched, 2, "本地头与中央目录各改一处");
        fs::write(&pkg, &b).unwrap();
        let declared = zip::ZipArchive::new(fs::File::open(&pkg).unwrap())
            .unwrap()
            .by_name(name)
            .unwrap()
            .size();
        assert_eq!(declared, 1024, "前筛看到的是撒谎的大小");
        preview_package(&pkg).expect("前筛按声明大小放行");

        let dst = t.path().join("themes/x");
        let err = import_package(&pkg, &dst, |_| Ok(()))
            .unwrap_err()
            .to_string();
        assert!(err.contains(name) && err.contains("超过上限"), "{err}");
        assert!(!dst.exists());
    }

    /// 符号链接不跟随：成环不死循环，指向主题目录外的文件不进包。
    #[cfg(unix)]
    #[test]
    fn export_skips_symlinks() {
        let t = tempfile::tempdir().unwrap();
        let src = t.path().join("s");
        fs::create_dir_all(src.join("assets")).unwrap();
        fs::write(src.join(THEME_ENTRY_NAME), THEME_TEXT).unwrap();
        fs::write(src.join("assets/bg.png"), b"bg").unwrap();
        let outside = t.path().join("secret.txt");
        fs::write(&outside, b"secret").unwrap();
        std::os::unix::fs::symlink(&outside, src.join("assets/leak.txt")).unwrap();
        std::os::unix::fs::symlink(src.join("assets"), src.join("assets/loop")).unwrap();
        std::os::unix::fs::symlink(t.path(), src.join("assets/up")).unwrap();

        let pkg = t.path().join("o.wtheme");
        export_package(&src, &pkg, None).unwrap();
        let names: Vec<String> = zip::ZipArchive::new(fs::File::open(&pkg).unwrap())
            .unwrap()
            .file_names()
            .map(String::from)
            .collect();
        assert!(names.contains(&"assets/bg.png".to_string()));
        assert!(
            names
                .iter()
                .all(|n| !n.contains("leak") && !n.contains("loop") && !n.contains("up/")),
            "{names:?}"
        );
    }
}
