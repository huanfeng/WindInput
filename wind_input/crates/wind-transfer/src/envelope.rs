//! 文本信封(`kind = "schema_text"`):一段 TOML 文本即一个完整分发包。
//!
//! 服务小方案(快符类:方案 + 小词库共 KB 级)的纯文本分发——剪贴板 / 文档站代码块
//! 即贴即装。分发格式,不是存储格式:拆解落盘后的形态与 zip 导入完全一致
//! (方案文件 + 词库文件),引擎与缓存管线零感知。
//!
//! ```toml
//! [package]
//! format_version = 2          # 必填。信封无 legacy——缺失即错
//! kind = "schema_text"        # 必填。显式声明,侦测不做猜测
//! title = "快符方案"           # 可选,说明元信息(语法与片段/zip 包同名同义)
//! description = """
//! 分号引导的符号表。
//! """
//!
//! [schema]                    # 可选冗余,免解析 files 即可显示 id/版本
//! id = "kf"
//! version = "1.00.0"
//!
//! [[files]]
//! path = "kf.schema.toml"     # schemas 根相对路径,逐条过 validate_entry_rel
//! content = '''…方案原文…'''
//! ```
//!
//! 显式声明才走此路:`[schema]` 也是合法的配置片段键前缀,裸方案文本不得被猜成信封。没有
//! `kind = "schema_text"` 的文本一律返回 [`NOT_SCHEMA_TEXT`] 前缀的错误,由调用方回落配置片段管线。

use serde::Deserialize;

use crate::scheme::{
    CONFIG_PATCH_NAME, PACKAGE_FORMAT_VERSION, PackageInfo, PackageMeta, PackageSchemaInfo,
    SchemeImportPreview,
};

/// 信封 kind 的唯一合法值。
pub const ENVELOPE_KIND: &str = "schema_text";

/// 「这段文本不是信封」错误的前缀。跨仓契约:设置端按此前缀判定要回落到配置片段
/// 管线(而不是把错误直接展示给用户)。改动须同步 wind-setting 的导入分派。
pub const NOT_SCHEMA_TEXT: &str = "not_schema_text:";

/// 信封文本体积上限(2 MB)。大方案走 zip/.wpkg,信封只服务小方案。
pub const MAX_TEXT_BYTES: usize = 2 * 1024 * 1024;

/// 信封文件条目数上限。
pub const MAX_FILES: usize = 64;

#[derive(Debug, Deserialize)]
struct Envelope {
    #[serde(default)]
    package: EnvelopePackage,
    #[serde(default)]
    schema: PackageSchemaInfo,
    #[serde(default)]
    files: Vec<EnvelopeFile>,
}

/// 信封的 `[package]` 段。`format_version` 用 `Option` 承载:「缺失」与「写了但过高」
/// 要报不同的话,回落默认值就分不清了。
///
/// `kind` 不在此列:它必须在反序列化之前判定(反序列化失败可能只因它不是信封),
/// 故由 [`parse`] 直接从 `toml::Value` 取。
#[derive(Debug, Default, Deserialize)]
struct EnvelopePackage {
    format_version: Option<u32>,
    #[serde(default)]
    app_version: String,
    #[serde(default)]
    platform: String,
    #[serde(default)]
    created_at: String,
    /// 说明元信息,语法与限额同片段与 zip 包(同名同义,一套语法学一次)。
    ///
    /// 与信封内 `config_patch.toml` 自带的 `[package]` 说明不合并:这里说的是
    /// "这个信封是什么",片段级说的是"这段配置改了什么"。导入确认对话框各显各的。
    #[serde(default)]
    title: String,
    #[serde(default)]
    description: String,
}

#[derive(Debug, Deserialize)]
struct EnvelopeFile {
    path: String,
    content: String,
}

/// 拆解后的信封:落盘载荷 + 元信息 + 配置片段原文。
struct ParsedEnvelope {
    meta: PackageMeta,
    /// (schemas 根相对路径, 文件内容字节),顺序 = 信封中的书写顺序。
    staged: Vec<(String, Vec<u8>)>,
    config_patch: Option<String>,
}

/// 解析并全量校验信封文本。
///
/// 校验顺序即报错优先级:先确认「是不是信封」(不是就让调用方回落片段管线),
/// 再查版本门禁,最后才是限额与布局。
fn parse(text: &str) -> anyhow::Result<ParsedEnvelope> {
    // 体积限额先于解析,且是硬错误、不带回落前缀:超 2 MB 的几乎必是超大信封而非手写片段;
    // 放在解析之后就挡不住它要挡的解析开销。
    if text.len() > MAX_TEXT_BYTES {
        anyhow::bail!(
            "方案文本信封过大({} 字节,上限 {MAX_TEXT_BYTES}),请改用 .wpkg 分发",
            text.len()
        );
    }
    // 非 TOML 文本同样不是信封:片段管线会给出更准确的解析错误。
    let value: toml::Value = text
        .parse::<toml::Value>()
        .map_err(|e| anyhow::anyhow!("{NOT_SCHEMA_TEXT} 不是合法 TOML 文本: {e}"))?;
    let kind = value
        .get("package")
        .and_then(|p| p.get("kind"))
        .and_then(|k| k.as_str());
    if kind != Some(ENVELOPE_KIND) {
        anyhow::bail!("{NOT_SCHEMA_TEXT} 缺少 package.kind = \"{ENVELOPE_KIND}\"");
    }
    // kind 已确认 → 后续一律是硬错误,不再回落片段管线。
    let env: Envelope = value
        .try_into()
        .map_err(|e| anyhow::anyhow!("方案文本信封无法解析: {e}"))?;

    let Some(format_version) = env.package.format_version else {
        anyhow::bail!("方案文本信封缺少 package.format_version(信封无 legacy,必须显式声明)");
    };
    if format_version > PACKAGE_FORMAT_VERSION {
        anyhow::bail!(
            "方案文本信封版本过高(format_version={format_version},当前支持 {PACKAGE_FORMAT_VERSION}),请升级 WindInput"
        );
    }
    // 说明元信息与版本同层(都是 `[package]` 段的声明)：走 wind-config 的同一份净化，
    // 不另写一份。非法即硬错误——说明会直接渲染进 UI。
    let mut package = PackageInfo {
        format_version,
        app_version: env.package.app_version,
        platform: env.package.platform,
        created_at: env.package.created_at,
        title: env.package.title,
        description: env.package.description,
    };
    crate::scheme::sanitize_package_info(&mut package)?;
    if env.files.len() > MAX_FILES {
        anyhow::bail!(
            "方案文本信封文件过多({} 个,上限 {MAX_FILES}),请改用 .wpkg 分发",
            env.files.len()
        );
    }

    let mut staged: Vec<(String, Vec<u8>)> = Vec::with_capacity(env.files.len());
    let mut config_patch: Option<String> = None;
    let mut has_root_schema = false;
    for f in &env.files {
        // 穿越守卫只此一处,与 zip 导入同一实现(components 白名单)。返回值已把 `\`
        // 归一为 `/`:判根与落盘都用它,否则 `sub\x.schema.toml` 会被当成根方案文件。
        let rel = crate::bundle::validate_entry_rel(&f.path, "")?;
        if staged.iter().any(|(r, _)| *r == rel)
            || (rel == CONFIG_PATCH_NAME && config_patch.is_some())
        {
            anyhow::bail!("方案文本信封含重复路径: {rel}");
        }
        // 配置片段按名识别、不落盘,语义与 zip 包内的 config_patch.toml 完全一致。
        if rel == CONFIG_PATCH_NAME {
            config_patch = Some(f.content.clone());
            continue;
        }
        if !rel.contains('/') && rel.ends_with(".schema.toml") {
            has_root_schema = true;
        }
        staged.push((rel, f.content.clone().into_bytes()));
    }
    if !has_root_schema {
        anyhow::bail!("不是有效的方案文本信封(根目录缺少 *.schema.toml)");
    }

    Ok(ParsedEnvelope {
        meta: PackageMeta {
            package,
            schema: env.schema,
            refs: Default::default(),
        },
        staged,
        config_patch,
    })
}

/// 信封导入预览(只读):will_add/conflicts 对 `user_dir` 计算,语义与 zip 版一致。
pub fn preview_import_text(
    text: &str,
    user_dir: &std::path::Path,
) -> anyhow::Result<SchemeImportPreview> {
    let p = parse(text)?;
    let rels: Vec<String> = p.staged.iter().map(|(r, _)| r.clone()).collect();
    Ok(crate::scheme::build_preview(
        p.meta,
        &rels,
        user_dir,
        p.config_patch,
    ))
}

/// 信封导入:校验期零落盘(全量解析通过才写),随后走与 zip 导入同一个写盘辅助
/// (逐文件 tmp+rename,Merge 跳过已存在)。配置片段不落盘。
pub fn import_text(
    text: &str,
    user_dir: &std::path::Path,
    strategy: crate::merge::Strategy,
) -> anyhow::Result<crate::scheme::SchemeImportResult> {
    let p = parse(text)?;
    let (imported, conflicts) = crate::scheme::write_staged(&p.staged, user_dir, strategy)?;
    let schema_ids = p
        .staged
        .iter()
        .map(|(r, _)| r.as_str())
        .filter(|r| !r.contains('/'))
        .filter_map(|r| r.strip_suffix(".schema.toml"))
        .map(String::from)
        .collect();
    Ok(crate::scheme::SchemeImportResult {
        imported,
        conflicts,
        schema_ids,
    })
}

#[cfg(test)]
#[path = "envelope_tests.rs"]
mod tests;
