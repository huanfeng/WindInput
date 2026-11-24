//! 方案包(scheme package)导出/导入 —— v2 简化格式。
//!
//! zip 条目名 = schemas 根相对路径(无目录前缀),导入零改写:
//! ```text
//! package.toml        可选元信息(TOML;导出恒写,导入不强制——兼容手工打包)
//! my.schema.toml      方案文件(根条目,识别方案 id 的依据)
//! my/main.dict.yaml   引用资源
//! ```
//! 三分类:用户目录命中→打包;系统目录命中→记 system 引用;均无→记 missing。
//! 「系统目录」是一组(`system_dirs`),按层序 `data_custom > data`——定制版自带的方案
//! 与出厂方案同类:导出时一并打包(自包含),删除时永不触碰。
//! 不再使用 bundle 层 manifest(方案文件自身已含 id/版本等大部分信息)。
use std::collections::HashSet;
use std::io::Write;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// 包内元信息文件名(可选条目)。
pub const PACKAGE_META_NAME: &str = "package.toml";

/// 当前实现支持的分发包格式版本(写进导出的 package.toml,导入按此门禁)。
pub const PACKAGE_FORMAT_VERSION: u32 = 2;

/// 包内配置片段条目名(根条目)。按名识别、不落盘:它不是方案资源,写进 schemas
/// 目录只会多一个死文件。文本随 [`SchemeImportPreview::config_patch`] 上浮,应用编排
/// 在设置端(导入方案文件 → `config.applyPatch`),文件层不复刻片段管线的热重载与镜像回灌。
pub const CONFIG_PATCH_NAME: &str = "config_patch.toml";

/// 包内条目数上限。
///
/// 真实方案包几十个条目顶天；两千给多套混输子方案留余量，同时挡掉只为拖死导入端的构造。
pub const MAX_PACKAGE_ENTRIES: usize = 2000;

/// 包内解压后总字节上限。大词库走的正是 .wpkg（可达几十 MB），故远大于文本信封的 2 MB；
/// 256 MiB 对真实方案包绰绰有余，同时挡住 zip 炸弹。
pub const MAX_PACKAGE_UNCOMPRESSED_BYTES: u64 = 256 * 1024 * 1024;

/// `package.toml` / `config_patch.toml` 这类手写文本条目的单条上限，与文本信封的
/// `MAX_TEXT_BYTES` 对齐。
const MAX_TEXT_ENTRY_BYTES: u64 = 2 * 1024 * 1024;

/// 缺 format_version 字段的包一律视为 legacy v1(该字段出现之前的产物)。
fn legacy_format_version() -> u32 {
    1
}

/// 方案包元信息(package.toml)。全部字段可缺省——导入端拿不到就显示"未知"。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PackageMeta {
    #[serde(default)]
    pub package: PackageInfo,
    #[serde(default)]
    pub schema: PackageSchemaInfo,
    #[serde(default)]
    pub refs: PackageRefs,
}

/// 导出环境信息 + 说明元信息。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PackageInfo {
    /// 包格式版本。缺省 = 1(legacy),导出恒写 PACKAGE_FORMAT_VERSION。
    #[serde(default = "legacy_format_version")]
    pub format_version: u32,
    #[serde(default)]
    pub app_version: String,
    #[serde(default)]
    pub platform: String,
    #[serde(default)]
    pub created_at: String,
    /// 包级说明标题(单行)。缺省即空串,导入端显示"未知"。
    ///
    /// 与包内 `config_patch.toml` 自带的 `[package]` 说明不合并:包级说「这个分发包是什么」,
    /// 片段级说「这段配置改了什么」,导入确认对话框各显各的。
    #[serde(default)]
    pub title: String,
    /// 包级说明正文(可多行)。与 [`Self::title`] 同源同校验,同样不与片段级说明合并。
    #[serde(default)]
    pub description: String,
}

impl Default for PackageInfo {
    fn default() -> Self {
        Self {
            format_version: legacy_format_version(),
            app_version: String::new(),
            platform: String::new(),
            created_at: String::new(),
            title: String::new(),
            description: String::new(),
        }
    }
}

/// 校验并归一 `[package]` 的说明元信息(title/description)。
///
/// 复用 wind-config 的同一份实现(片段 / `package.toml` / 文本信封三种载体同名同义)。
/// 失败是与 `format_version` 门禁同层的硬错误:说明直接渲染进 UI,不静默截断或丢弃。
pub(crate) fn sanitize_package_info(info: &mut PackageInfo) -> anyhow::Result<()> {
    info.title =
        wind_config::patch::sanitize_title(&info.title).map_err(|e| anyhow::anyhow!("{e}"))?;
    info.description = wind_config::patch::sanitize_description(&info.description)
        .map_err(|e| anyhow::anyhow!("{e}"))?;
    Ok(())
}

/// 根方案标识(与 zip 内 .schema.toml 冗余,便于免解压显示)。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PackageSchemaInfo {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub version: String,
}

/// 引用清单:系统种子引用(不打包)与导出时即缺失的引用。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PackageRefs {
    #[serde(default)]
    pub system: Vec<String>,
    #[serde(default)]
    pub missing: Vec<String>,
}

/// 收集计划:待打包文件(schemas 相对路径, 源绝对路径)、系统引用、缺失、
/// 涉及的方案 id(根在前)、根方案版本(schema.toml 未标注则空)。
pub struct CollectPlan {
    pub pack: Vec<(String, PathBuf)>,
    /// override 折叠产物:方案文件 rel → 合并后序列化的 TOML 内容。仅当该方案存在
    /// `schema_overrides/{id}.toml` 时才有条目;写 zip 时此内容优先于读源文件字节——
    /// 包自含定制后的方案,导入方无需 override 机制即得到定制后行为。
    pub folded: Vec<(String, String)>,
    pub system_refs: Vec<String>,
    pub missing: Vec<String>,
    pub schema_ids: Vec<String>,
    pub root_version: String,
}

/// 单个相对路径的三分类结果。System 携带源绝对路径,自包含导出需据此读取打包。
enum Located {
    User(PathBuf),
    System(PathBuf),
    Missing,
}

/// 方案文件里写下的相对路径安全可用吗（不逃出 schemas 根）。
///
/// [`crate::bundle::validate_entry_rel`] 只校验归档条目名，而方案文件内容（`dictionaries[].path`、
/// `engine.chaizi.db_path` / `font_path`、`shuangpin.layout`、`engine.mixed.*_schema`）同样是
/// 包带进来的外来数据，会被 `user_dir.join()` 当路径用。没有这道守卫，写 `../victim.txt` 的
/// 方案会在删除时删掉 user_dir 之外的文件、导出时把它打进包。
///
/// 判据直接复用 `validate_entry_rel`，不另写一份（两份谓词迟早漂移）。只取 Ok/Err，归一化后的
/// 返回值刻意丢弃，以免改写 `plan.pack` 里的 rel 字符串。
fn is_safe_rel(rel: &str) -> bool {
    crate::bundle::validate_entry_rel(rel, "").is_ok()
}

/// 定位一个方案资源。不安全的 rel 一律当 Missing（调用方记进 `plan.missing`，既不打包也不删除）。
///
/// `system_dirs` 是按层序排列的一组系统 schemas 目录（`data_custom` 在前、`data` 在后），漏传
/// 定制层会把定制版方案判成 `Missing`。定制层归 `System` 而不是 `User` 是刚性的：同一个
/// `locate` 也服务删除路径，判成 `User` 就等于允许程序删 `data_custom` 里的文件。
fn locate(rel: &str, user_dir: &Path, system_dirs: &[PathBuf]) -> Located {
    if !is_safe_rel(rel) {
        // 归到 Missing 而不是静默丢弃：`plan.missing` 会一路上送到设置页的缺失清单，已足够
        // 指认问题。不在这里打日志：本 crate 零日志依赖。
        return Located::Missing;
    }
    let u = user_dir.join(rel);
    if u.is_file() {
        return Located::User(u);
    }
    for s in system_dirs {
        let sp = s.join(rel);
        if sp.is_file() {
            return Located::System(sp);
        }
    }
    Located::Missing
}

/// `wubi86/x.dict.yaml` → `wubi86/x.wdat`（包内相对路径版）。
///
/// 与 `wind_dict::cached::wdat_sibling` 同一套命名约定，但不复用它：那个走 `Path`，Windows
/// 上会把分隔符规范化成 `\`，而包内相对路径必须保持 `/`。两处若要改约定需同步。
fn wdat_rel(rel: &str) -> Option<String> {
    let stem = rel
        .strip_suffix(".dict.yaml")
        .or_else(|| rel.strip_suffix(".yaml"))?;
    Some(format!("{stem}.wdat"))
}

/// 资源定位：先按 `rel` 本身找，未命中再按 wdat-only 约定找同名 `.wdat`。
///
/// 返回实际命中的相对路径（wdat-only 时改写成 `.wdat`），否则导入端会还原出名为 yaml、
/// 内容却是二进制的文件。
///
/// 非词库资源（字根字体、shuangpin toml 等）不受影响——`wdat_sibling` 对非 yaml 后缀
/// 返回 `None`，直接落回原结果。
fn locate_resource(rel: &str, user_dir: &Path, system_dirs: &[PathBuf]) -> (String, Located) {
    match locate(rel, user_dir, system_dirs) {
        Located::Missing => {
            if let Some(w) = wdat_rel(rel) {
                match locate(&w, user_dir, system_dirs) {
                    Located::Missing => {}
                    hit => return (w, hit),
                }
            }
            // 仍未命中：用原始 rel 上报，缺失提示才对得上用户配置里写的路径
            (rel.to_string(), Located::Missing)
        }
        hit => (rel.to_string(), hit),
    }
}

/// 从一个已解析 Schema 提取其资源相对路径(不含方案文件本身、不含引用方案)。
fn resource_rels(schema: &wind_config::schema::Schema) -> Vec<String> {
    let mut rels = Vec::new();
    for d in &schema.dictionaries {
        if !d.path.is_empty() {
            rels.push(d.path.clone());
        }
    }
    let cz = &schema.engine.chaizi;
    if !cz.db_path.is_empty() {
        rels.push(cz.db_path.clone());
    }
    if !cz.font_path.is_empty() {
        rels.push(cz.font_path.clone());
    }
    let py = &schema.engine.pinyin;
    // （unigram.txt 已不在收集之列：引擎侧读取链移除后它不再随 data/ 分发，
    // 词图打分改用词条自身的词典权重。老方案包里若带着它，解包时多出一个无人读的
    // 文件，无害。）
    if !py.shuangpin.layout.is_empty() {
        rels.push(format!("shuangpin/{}.toml", py.shuangpin.layout));
    }
    rels
}

/// 读取 `override_dir/{id}.toml`(设置页方案定制层)。文件不存在 → `Ok(None)`;
/// 存在但读不了/解析失败 → 硬报错。导出场景静默按无 override 处理 = 用户以为定制
/// 随包带走了,实际导出的是未定制方案——宁可导出失败也不许无声丢定制。
fn read_override_value(
    id: &str,
    override_dir: Option<&Path>,
) -> anyhow::Result<Option<toml::Value>> {
    let Some(dir) = override_dir else {
        return Ok(None);
    };
    // id 同样来自方案文件内容（`engine.mixed.*_schema`），照样要过守卫。
    //
    // 当前调用序上 `collect_into` 已先校验过，但那是调用顺序给的保证，不是本函数的契约。
    if !is_safe_rel(&format!("{id}.toml")) {
        anyhow::bail!("非法方案 id（越界路径）: {id}");
    }
    let path = dir.join(format!("{id}.toml"));
    if !path.is_file() {
        return Ok(None);
    }
    let text = std::fs::read_to_string(&path)
        .map_err(|e| anyhow::anyhow!("读取方案 override 失败 {}: {e}", path.display()))?;
    let v: toml::Value = text
        .parse::<toml::Value>()
        .map_err(|e| anyhow::anyhow!("方案 override 无法解析 {}: {e}", path.display()))?;
    Ok(Some(v))
}

/// 收集方案 `id` 的打包计划。根方案文件必打包(用户目录优先解析,系统命中也打包——
/// 包必须自含方案文件);引用的用户方案递归(visited 防环)。
///
/// `include_system` 控制系统目录命中的资源/子方案如何处理:
/// - `true`(自包含导出):一并读源打包 → 产出的包在任何机器上都完整可用,不依赖目标机内置文件。
/// - `false`(删除路径复用):系统命中只记 `system_refs` 且子方案不递归,维持"系统文件永不触碰"的语义。
///
/// `override_dir` = 用户配置目录下的 `schema_overrides`(设置页定制层)。给定时,每个方案
/// (含 mixed 递归的子方案)各自与其 override 深合并后再收集——override 新指向的布局/词库/
/// 字体因此被收集,合并结果记入 `folded` 供导出写包。删除路径传 `None`:删除收集语义不折叠。
pub fn collect_package_files(
    id: &str,
    user_dir: &Path,
    system_dirs: &[PathBuf],
    override_dir: Option<&Path>,
    include_system: bool,
) -> anyhow::Result<CollectPlan> {
    let mut plan = CollectPlan {
        pack: Vec::new(),
        folded: Vec::new(),
        system_refs: Vec::new(),
        missing: Vec::new(),
        schema_ids: Vec::new(),
        root_version: String::new(),
    };
    let mut visited: HashSet<String> = HashSet::new();
    collect_into(
        id,
        true,
        include_system,
        user_dir,
        system_dirs,
        override_dir,
        &mut plan,
        &mut visited,
    )?;
    Ok(plan)
}

#[allow(clippy::too_many_arguments)]
fn collect_into(
    id: &str,
    is_root: bool,
    include_system: bool,
    user_dir: &Path,
    system_dirs: &[PathBuf],
    override_dir: Option<&Path>,
    plan: &mut CollectPlan,
    visited: &mut HashSet<String>,
) -> anyhow::Result<()> {
    if !visited.insert(id.to_string()) {
        return Ok(()); // 防环
    }
    let schema_rel = format!("{id}.schema.toml");
    // 方案文件解析:用户优先;根方案系统命中必打包(包自含);非根系统命中在自包含模式下也
    // 打包并继续递归其资源,否则只记引用不递归(删除路径:系统文件永不触碰)。
    let schema_abs = match locate(&schema_rel, user_dir, system_dirs) {
        Located::User(p) => p,
        Located::System(p) => {
            if is_root || include_system {
                p
            } else {
                plan.system_refs.push(schema_rel);
                return Ok(());
            }
        }
        Located::Missing => {
            if is_root {
                anyhow::bail!("方案文件不存在: {}", schema_rel);
            }
            plan.missing.push(schema_rel);
            return Ok(());
        }
    };
    plan.schema_ids.push(id.to_string());
    plan.pack.push((schema_rel.clone(), schema_abs.clone()));

    let text = std::fs::read_to_string(&schema_abs)?;
    // override 折叠:存在则把设置页写的稀疏 diff 深合并进方案文件(与引擎加载
    // `read_schema` 同一实现),资源收集与入包内容都以合并视图为准。无 override 时
    // 不走 Value 往返,方案文件字节原样入包(既有测试守护字节级不变)。
    let schema: wind_config::schema::Schema = match read_override_value(id, override_dir)? {
        Some(ov) => {
            let mut base: toml::Value = text.parse()?;
            wind_config::schema::merge_toml(&mut base, ov);
            let folded_text = toml::to_string_pretty(&base)?;
            let schema = wind_config::schema::Schema::from_toml_value(base)
                .map_err(|e| anyhow::anyhow!("方案 {id} 与 override 合并后解析失败: {e}"))?;
            plan.folded.push((schema_rel, folded_text));
            schema
        }
        // 与上一支同走 Value：直接 from_str::<Schema> 会让整族方案类型再单态化一份。
        None => wind_config::schema::Schema::from_toml_value(text.parse()?)?,
    };
    if is_root {
        plan.root_version = schema.schema.version.clone();
    }

    for rel in resource_rels(&schema) {
        // 词库可能是 wdat-only（只有编译好的 .wdat、无 .dict.yaml 源），实际打包的相对
        // 路径要以命中者为准，见 locate_resource。
        let (rel, located) = locate_resource(&rel, user_dir, system_dirs);
        match located {
            Located::User(p) => plan.pack.push((rel, p)),
            // 自包含导出:系统词库/拆字库/字体也读源打包;删除路径:只记引用不打包。
            Located::System(p) => {
                if include_system {
                    plan.pack.push((rel, p));
                } else {
                    plan.system_refs.push(rel);
                }
            }
            Located::Missing => plan.missing.push(rel),
        }
    }
    // mixed 引用方案:用户命中→递归;系统命中→自包含模式递归打包,否则记引用;缺失→记缺失。
    let mixed = &schema.engine.mixed;
    for sub in [&mixed.primary_schema, &mixed.secondary_schema] {
        if !sub.is_empty() {
            collect_into(
                sub,
                false,
                include_system,
                user_dir,
                system_dirs,
                override_dir,
                plan,
                visited,
            )?;
        }
    }
    Ok(())
}

/// 导出结果(RPC 直接序列化消费)。packed 为 schemas 相对路径。
pub struct SchemeExportResult {
    pub path: PathBuf,
    pub packed: Vec<String>,
    pub system_refs: Vec<String>,
    pub missing: Vec<String>,
}

/// 导出方案包:收集 → 写 zip(package.toml 元信息 + 各文件按 schemas 相对路径直存)。
///
/// `override_dir` 见 [`collect_package_files`]:给定时各方案与 `schema_overrides/{id}.toml`
/// 折叠后入包(自包含定制),无 override 的方案文件字节原样入包。
#[allow(clippy::too_many_arguments)]
pub fn export_package(
    id: &str,
    user_dir: &Path,
    system_dirs: &[PathBuf],
    override_dir: Option<&Path>,
    out_path: &Path,
    app_version: &str,
    platform: &str,
    created_at: &str,
) -> anyhow::Result<SchemeExportResult> {
    // 自包含导出:内置(系统目录)词库/拆字/字体一并打包,产出的包脱离目标机内置文件也完整可用。
    let plan = collect_package_files(id, user_dir, system_dirs, override_dir, true)?;
    let meta = PackageMeta {
        package: PackageInfo {
            format_version: PACKAGE_FORMAT_VERSION,
            app_version: app_version.to_string(),
            platform: platform.to_string(),
            created_at: created_at.to_string(),
            // 导出留空:导出的是用户自己的方案,没有"说明"这个来源(它是分发者写给
            // 收方看的话)。手工打包/web 工具写进来的值照样能原样读回。
            title: String::new(),
            description: String::new(),
        },
        schema: PackageSchemaInfo {
            id: id.to_string(),
            version: plan.root_version.clone(),
        },
        refs: PackageRefs {
            system: plan.system_refs.clone(),
            missing: plan.missing.clone(),
        },
    };

    if let Some(parent) = out_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let file = std::fs::File::create(out_path)?;
    let mut w = zip::ZipWriter::new(file);
    let opts = zip::write::SimpleFileOptions::default();
    w.start_file(PACKAGE_META_NAME, opts)?;
    w.write_all(toml::to_string_pretty(&meta)?.as_bytes())?;
    let mut packed = Vec::new();
    // 有 override 的方案文件写折叠后内容(定制随包自含),其余条目原样读源字节。
    let folded: std::collections::HashMap<&str, &str> = plan
        .folded
        .iter()
        .map(|(rel, content)| (rel.as_str(), content.as_str()))
        .collect();
    for (rel, src) in &plan.pack {
        w.start_file(rel, opts)?;
        match folded.get(rel.as_str()) {
            Some(content) => w.write_all(content.as_bytes())?,
            None => w.write_all(&std::fs::read(src)?)?,
        }
        packed.push(rel.clone());
    }
    w.finish()?;
    Ok(SchemeExportResult {
        path: out_path.to_path_buf(),
        packed,
        system_refs: plan.system_refs,
        missing: plan.missing,
    })
}

/// 包内一个载荷条目。
///
/// `name` = zip 条目原名(取字节只能用它);`rel` = 归一化后的 schemas 相对路径
/// (落盘、判根、取 id 只能用它)。两者仅在条目名含 `\` 的非规范包里不同,混用其一即错。
pub(crate) struct PayloadEntry {
    pub name: String,
    pub rel: String,
}

/// 枚举包内条目(排除 package.toml、目录项),逐条过穿越守卫,返回(落盘载荷条目, 根
/// `config_patch.toml` 的 zip 原名)。校验根下至少一个 `*.schema.toml`;对备份包/旧格式给出
/// 针对性错误。版本门禁在 [`read_package_meta`] 里,恒先于布局校验与说明校验。
///
/// config_patch 按名识别后从载荷里摘出去:它既不进 will_add/conflicts,也不参与
/// 「根下有没有 *.schema.toml」的判定(纯配置包由设置端的侦测规则 3 分派,不走这里),
/// 更不会被 `import_package` 写进 schemas 目录。
///
/// 它还要求 `format_version ≥ 2`:legacy 语义下旧客户端会把它当死文件落盘,生成端
/// 必须显式声明版本——硬拒绝比「装了但配置没生效」更早暴露问题。
fn scan_package(package: &Path) -> anyhow::Result<(Vec<PayloadEntry>, Option<String>)> {
    scan_package_with_limit(package, MAX_PACKAGE_UNCOMPRESSED_BYTES)
}

/// [`scan_package`] 的可调上限版：声明总大小超过 `max_uncompressed` 即拒绝。
fn scan_package_with_limit(
    package: &Path,
    max_uncompressed: u64,
) -> anyhow::Result<(Vec<PayloadEntry>, Option<String>)> {
    // 版本门禁在 read_package_meta 内(先于说明校验),此处不再重复。
    let meta = read_package_meta(package)?;
    let file = std::fs::File::open(package)?;
    let mut archive = zip::ZipArchive::new(file)?;
    // 规模门禁：只读中央目录、不解压，故排在逐条校验之前。这里的大小是归档自己声明的，
    // 骗得过；真正的界在 `import_package` 的逐条有界读取（`extract_entry_limited`）。
    if archive.len() > MAX_PACKAGE_ENTRIES {
        anyhow::bail!(
            "方案包条目过多({} 个,上限 {MAX_PACKAGE_ENTRIES})",
            archive.len()
        );
    }
    let mut declared: u64 = 0;
    for i in 0..archive.len() {
        declared = declared.saturating_add(archive.by_index_raw(i)?.size());
    }
    if declared > max_uncompressed {
        anyhow::bail!("方案包解压后过大(声明 {declared} 字节,上限 {max_uncompressed})");
    }
    let mut entries: Vec<PayloadEntry> = Vec::new();
    let mut has_root_schema = false;
    let mut config_patch_name: Option<String> = None;
    let names: Vec<String> = archive.file_names().map(String::from).collect();
    for name in &names {
        let name = name.as_str();
        if name == PACKAGE_META_NAME || name.ends_with('/') {
            continue;
        }
        if name == "manifest.toml" || name == "manifest.json" {
            anyhow::bail!("该文件是整机备份包或旧格式归档,不是方案包");
        }
        // 判根/落盘一律用归一化值:`sub\evil.schema.toml` 的 `contains('/')` 为 false,
        // 拿原名判根会把子目录文件当成根方案文件。
        let rel = crate::bundle::validate_entry_rel(name, "")?;
        if rel == CONFIG_PATCH_NAME {
            config_patch_name = Some(name.to_string());
            continue;
        }
        if !rel.contains('/') && rel.ends_with(".schema.toml") {
            has_root_schema = true;
        }
        entries.push(PayloadEntry {
            name: name.to_string(),
            rel,
        });
    }
    if config_patch_name.is_some() && meta.package.format_version < 2 {
        anyhow::bail!(
            "包内含 {CONFIG_PATCH_NAME} 却声明 format_version={},配置片段需 format_version = 2,请按新格式重新打包",
            meta.package.format_version
        );
    }
    if !has_root_schema {
        anyhow::bail!("不是有效的方案包(根目录缺少 *.schema.toml)");
    }
    entries.sort_by(|a, b| a.rel.cmp(&b.rel));
    Ok((entries, config_patch_name))
}

/// 读取包内可选元信息,并按序做版本门禁 → 说明校验两道检查。
///
/// 解析的宽容度(宽容只给过去,不给未来):
/// - `package.toml` 不存在 → 回落默认值(导入不强制元信息,兼容手工打包);
/// - 解析失败但原文声明了 `package.format_version` → 硬报错——声明新格式却写坏,
///   静默回落会把 format_version 当 legacy 处理,等于绕过版本门禁;
/// - 解析失败且无该字段(或根本不是 TOML)→ 回落默认值(legacy 宽容,现状不变)。
///
/// 两道检查的先后不可换:更高版本的包说明规则可能已变,「版本过高,请升级」才对得上真实原因。
/// 信封路径(`envelope::parse`)必须同一顺序。
pub fn read_package_meta(package: &Path) -> anyhow::Result<PackageMeta> {
    // 有界读取：package.toml 是手写 TOML，KB 量级。读不出来（不存在 / 超上限）一律
    // 回落默认值——超大的 package.toml 只可能是构造出来的，而 legacy 宽容语义本就是
    // 「读不到元信息就当没有」，在这里区分两种读不到没有意义。
    let Ok(bytes) =
        crate::bundle::extract_entry_limited(package, PACKAGE_META_NAME, MAX_TEXT_ENTRY_BYTES)
    else {
        return Ok(PackageMeta::default());
    };
    let text = String::from_utf8_lossy(&bytes);
    match toml::from_str::<PackageMeta>(&text) {
        Ok(mut meta) => {
            if meta.package.format_version > PACKAGE_FORMAT_VERSION {
                anyhow::bail!(
                    "方案包版本过高(format_version={},当前支持 {}),请升级 WindInput",
                    meta.package.format_version,
                    PACKAGE_FORMAT_VERSION
                );
            }
            // 说明校验在版本门禁之后:preview 与 import 都必经本函数,是两条路径的共用点。
            sanitize_package_info(&mut meta.package)?;
            Ok(meta)
        }
        Err(e) => {
            let declares_format_version = text
                .parse::<toml::Value>()
                .ok()
                .and_then(|v| v.get("package")?.get("format_version").cloned())
                .is_some();
            if declares_format_version {
                anyhow::bail!("package.toml 无法解析: {e}");
            }
            Ok(PackageMeta::default())
        }
    }
}

/// 从条目清单提取方案 id(根下 `*.schema.toml` 的文件名主干)。
fn schema_ids_of(rels: &[String]) -> Vec<String> {
    rels.iter()
        .filter(|r| !r.contains('/'))
        .filter_map(|r| r.strip_suffix(".schema.toml"))
        .map(String::from)
        .collect()
}

/// 导入预览(只读):按包内条目对目标目录做存在性检查。
#[derive(Debug)]
pub struct SchemeImportPreview {
    pub meta: PackageMeta,
    pub will_add: Vec<String>,
    pub conflicts: Vec<String>,
    pub system_refs: Vec<String>,
    pub missing: Vec<String>,
    /// 包内 `config_patch.toml` 原文(不落盘)。上层据此生成逐键 diff 并在
    /// 导入成功后另调 `config.applyPatch`——见 [`CONFIG_PATCH_NAME`]。
    pub config_patch: Option<String>,
}

pub fn preview_import(package: &Path, user_dir: &Path) -> anyhow::Result<SchemeImportPreview> {
    let (entries, config_patch_name) = scan_package(package)?;
    let meta = read_package_meta(package)?;
    let config_patch = read_config_patch(package, config_patch_name.as_deref())?;
    let rels: Vec<String> = entries.into_iter().map(|e| e.rel).collect();
    Ok(build_preview(meta, &rels, user_dir, config_patch))
}

/// 取包内配置片段原文。按 zip 原名读——归一化后的名字回查 zip 会找不到条目。
fn read_config_patch(package: &Path, name: Option<&str>) -> anyhow::Result<Option<String>> {
    let Some(name) = name else {
        return Ok(None);
    };
    // 有界读取：配置片段是手写 TOML。这一条在 `preview_import`（只读预览）里就会被读，
    // 不设界的话「只是看一眼这个包」都能被撑爆。
    let bytes = crate::bundle::extract_entry_limited(package, name, MAX_TEXT_ENTRY_BYTES)?;
    Ok(Some(String::from_utf8(bytes).map_err(|_| {
        anyhow::anyhow!("{CONFIG_PATCH_NAME} 不是 UTF-8 文本")
    })?))
}

/// 由条目清单对目标目录算出 will_add/conflicts,组装预览。zip 与文本信封共用,
/// 保证两条导入路径的预览语义逐字段一致。
pub(crate) fn build_preview(
    meta: PackageMeta,
    rels: &[String],
    user_dir: &Path,
    config_patch: Option<String>,
) -> SchemeImportPreview {
    let mut will_add = Vec::new();
    let mut conflicts = Vec::new();
    for rel in rels {
        if user_dir.join(rel).exists() {
            conflicts.push(rel.clone());
        } else {
            will_add.push(rel.clone());
        }
    }
    let system_refs = meta.refs.system.clone();
    let missing = meta.refs.missing.clone();
    SchemeImportPreview {
        meta,
        will_add,
        conflicts,
        system_refs,
        missing,
        config_patch,
    }
}

/// 导入结果。
pub struct SchemeImportResult {
    pub imported: Vec<String>,
    pub conflicts: Vec<String>,
    pub schema_ids: Vec<String>,
}

/// 导入方案包到用户 schemas 目录。先全部读入内存(校验期零落盘),再逐文件 tmp+rename。
/// Merge=已存在跳过(计 conflicts);Replace=覆盖。
pub fn import_package(
    package: &Path,
    user_dir: &Path,
    strategy: crate::merge::Strategy,
) -> anyhow::Result<SchemeImportResult> {
    import_package_with_limit(package, user_dir, strategy, MAX_PACKAGE_UNCOMPRESSED_BYTES)
}

/// [`import_package`] 的可调上限版（内存吃紧的平台用更低的解压后总量上限）。
/// 上限同时约束归档声明的总大小与逐条有界读取的实际余额，撒谎的归档也越不过去。
/// `import_package` 恒用 [`MAX_PACKAGE_UNCOMPRESSED_BYTES`]，行为不变。
pub fn import_package_with_limit(
    package: &Path,
    user_dir: &Path,
    strategy: crate::merge::Strategy,
    max_uncompressed: u64,
) -> anyhow::Result<SchemeImportResult> {
    let (entries, _) = scan_package_with_limit(package, max_uncompressed)?;
    // 读取阶段:全部载荷入内存,任何缺条目/坏条目在写盘前失败。
    // 取字节按 zip 原名,落盘按归一化 rel——见 [`PayloadEntry`]。
    let mut staged: Vec<(String, Vec<u8>)> = Vec::new();
    // 逐条有界读取，且预算是整包共享的余额——这才是真正的界：`scan_package` 那道
    // 前筛读的是归档自己声明的大小，撒谎的包骗得过它（声明 1 KB、解压 10 GB）。
    let mut remaining = max_uncompressed;
    for e in &entries {
        let bytes = crate::bundle::extract_entry_limited(package, &e.name, remaining)?;
        remaining -= bytes.len() as u64;
        staged.push((e.rel.clone(), bytes));
    }
    let rels: Vec<String> = entries.into_iter().map(|e| e.rel).collect();
    // 写入阶段:tmp+rename,Merge 跳过已存在。
    let (imported, conflicts) = write_staged(&staged, user_dir, strategy)?;
    Ok(SchemeImportResult {
        imported,
        conflicts,
        schema_ids: schema_ids_of(&rels),
    })
}

/// 把已读入内存的载荷落进用户 schemas 目录:逐文件 tmp+rename,Merge 跳过已存在
/// (计 conflicts),Replace 覆盖。返回 (imported, conflicts)。
///
/// zip 导入与文本信封导入共用——第二份写盘逻辑就是第二份真相源,原子性/冲突语义
/// 会各自漂移。
pub(crate) fn write_staged(
    staged: &[(String, Vec<u8>)],
    user_dir: &Path,
    strategy: crate::merge::Strategy,
) -> anyhow::Result<(Vec<String>, Vec<String>)> {
    let mut imported = Vec::new();
    let mut conflicts = Vec::new();
    for (rel, bytes) in staged {
        let target = user_dir.join(rel);
        if target.exists() && strategy == crate::merge::Strategy::Merge {
            conflicts.push(rel.clone());
            continue;
        }
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let tmp = target.with_extension("windinput.tmp");
        std::fs::write(&tmp, bytes)?;
        // 包里可以只带 sidecar `.wdat`(见 `wdat_rel`),而那种词库是直接走
        // `wind_dict::reader_pool` 打开的——正在运行的引擎手里可能还攥着指向旧内容的
        // mmap。池的 `(大小, mtime)` 判据在「大小不变 + 同一 mtime 刻度」时认不出替换,
        // 故这里也要走替换协议。守卫圈住「先删后 rename」整段。
        let replacing = wind_dict::reader_pool::is_pooled(&target)
            .then(|| wind_dict::reader_pool::replacing(&target));
        let renamed = (|| {
            // Windows: rename 到已存在目标会失败,Replace 先移除旧文件。
            if target.exists() {
                std::fs::remove_file(&target)?;
            }
            std::fs::rename(&tmp, &target)
        })();
        drop(replacing);
        renamed?;
        imported.push(rel.clone());
    }
    Ok((imported, conflicts))
}

/// 删除结果。rel 均为 schemas 根相对路径。
pub struct SchemeDeleteResult {
    /// 实际删除的文件。
    pub deleted: Vec<String>,
    /// 因被其它方案共享而保留的文件。
    pub kept_shared: Vec<String>,
    /// 方案文件被实际删除的方案 id(根在前;共享保留的子方案不计)。
    pub schema_ids: Vec<String>,
}

/// 删除用户方案 —— 镜像导入的收集逻辑:方案文件 + 引用资源 + 递归引用的用户方案。
/// 被 `keep_ids` 中任一现存方案引用的文件保留(共享检查,如混输共用的子方案/码表);
/// 系统目录文件永不删;删除后自底向上清理空目录(至 user_dir 止)。
pub fn delete_package(
    id: &str,
    user_dir: &Path,
    system_dirs: &[PathBuf],
    keep_ids: &[String],
) -> anyhow::Result<SchemeDeleteResult> {
    // 删除只关心用户目录文件,系统命中记引用即可(include_system=false),避免解析系统子方案。
    // 不折叠 override(传 None):删除按方案文件本身的引用收集,语义与历史行为字节级一致;
    // 与导出侧的折叠不对称是刻意的,这轮不动删除的收集语义。
    let plan = collect_package_files(id, user_dir, system_dirs, None, false)?;
    // 其余现存方案引用的文件集合(单个方案收集失败不阻断删除,跳过即可)。
    let mut kept: HashSet<String> = HashSet::new();
    for kid in keep_ids {
        if kid == id {
            continue;
        }
        if let Ok(p) = collect_package_files(kid, user_dir, system_dirs, None, false) {
            kept.extend(p.pack.into_iter().map(|(rel, _)| rel));
        }
    }
    let mut deleted = Vec::new();
    let mut kept_shared = Vec::new();
    let mut deleted_rels: HashSet<String> = HashSet::new();
    // 「在 user_dir 之下吗」必须规范化之后再比：`Path::starts_with` 是组件前缀比较，
    // `user_dir/../../x` 恒返回 true。上游 `is_safe_rel` 已挡掉越界 rel，这里是纵深第二道
    // （删除不可逆）。canonicalize 失败即 bail，拿不准就别删。
    let user_root = user_dir
        .canonicalize()
        .map_err(|e| anyhow::anyhow!("无法规范化用户方案目录 {}: {e}", user_dir.display()))?;
    for (rel, src) in &plan.pack {
        // 系统目录文件(如内置根方案)永不删；规范化失败(文件已消失/无权限)同样不删。
        let inside = src
            .canonicalize()
            .map(|c| c.starts_with(&user_root))
            .unwrap_or(false);
        if !inside {
            continue;
        }
        if kept.contains(rel) {
            kept_shared.push(rel.clone());
            continue;
        }
        std::fs::remove_file(src)?;
        deleted_rels.insert(rel.clone());
        deleted.push(rel.clone());
        // 自底向上清理空目录(remove_dir 对非空目录失败即停)。
        let mut dir = src.parent();
        while let Some(d) = dir {
            if d == user_dir || std::fs::remove_dir(d).is_err() {
                break;
            }
            dir = d.parent();
        }
    }
    let schema_ids = plan
        .schema_ids
        .iter()
        .filter(|sid| deleted_rels.contains(&format!("{sid}.schema.toml")))
        .cloned()
        .collect();
    Ok(SchemeDeleteResult {
        deleted,
        kept_shared,
        schema_ids,
    })
}

#[cfg(test)]
#[path = "scheme_tests.rs"]
mod tests;
