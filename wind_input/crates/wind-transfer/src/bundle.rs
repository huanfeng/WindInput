use serde::{Deserialize, Serialize};
use std::io::{Read, Write};
use std::path::Path;

pub const SPEC_VERSION: u32 = 1;
pub const FORMAT_TAG: &str = "windinput-bundle";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum BundleKind {
    Scheme,
    Backup,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContentEntry {
    pub r#type: String,
    pub path: String,
    // TOML 无 null:Null 序列化时省略;反序列化缺省回落 Null(serde_json::Value 的默认值)。
    #[serde(default, skip_serializing_if = "serde_json::Value::is_null")]
    pub meta: serde_json::Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Manifest {
    pub format: String,
    pub kind: BundleKind,
    pub spec_version: u32,
    pub app_version: String,
    pub platform: String,
    pub created_at: String,
    #[serde(default)]
    pub contents: Vec<ContentEntry>,
}

impl Manifest {
    pub fn new(kind: BundleKind, app_version: &str, platform: &str, created_at: &str) -> Self {
        Self {
            format: FORMAT_TAG.to_string(),
            kind,
            spec_version: SPEC_VERSION,
            app_version: app_version.to_string(),
            platform: platform.to_string(),
            created_at: created_at.to_string(),
            contents: Vec::new(),
        }
    }

    /// 校验:format 与 FORMAT_TAG 一致,且 spec_version 不高于当前支持版本。
    pub fn validate(&self) -> anyhow::Result<()> {
        if self.format != FORMAT_TAG {
            anyhow::bail!("非 WindInput 归档(format={})", self.format);
        }
        if self.spec_version > SPEC_VERSION {
            anyhow::bail!(
                "归档版本过高(spec_version={},当前支持 {}),请升级 WindInput",
                self.spec_version,
                SPEC_VERSION
            );
        }
        Ok(())
    }
}

const MANIFEST_NAME: &str = "manifest.toml";

pub struct BundleWriter {
    writer: zip::ZipWriter<std::fs::File>,
    manifest: Manifest,
}

impl BundleWriter {
    pub fn new(path: &Path, manifest: Manifest) -> anyhow::Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let file = std::fs::File::create(path)?;
        Ok(Self {
            writer: zip::ZipWriter::new(file),
            manifest,
        })
    }

    /// 写入一个条目,并在 manifest.contents 里登记(type 由调用方后续细化;空 type 为 P1 兼容)。
    pub fn add_bytes(&mut self, name: &str, data: &[u8]) -> anyhow::Result<()> {
        self.add_bytes_with(name, data, "", serde_json::Value::Null)
    }

    /// 写入条目并按给定 type/meta 登记(P3:schema/resource 等类型化条目)。
    pub fn add_bytes_with(
        &mut self,
        name: &str,
        data: &[u8],
        r#type: &str,
        meta: serde_json::Value,
    ) -> anyhow::Result<()> {
        self.writer
            .start_file(name, zip::write::SimpleFileOptions::default())?;
        self.writer.write_all(data)?;
        self.manifest.contents.push(ContentEntry {
            r#type: r#type.to_string(),
            path: name.to_string(),
            meta,
        });
        Ok(())
    }

    /// 只登记 manifest 的引用条目,不写 zip 载荷(system_ref/missing 等)。
    pub fn add_ref(&mut self, r#type: &str, path: &str, meta: serde_json::Value) {
        self.manifest.contents.push(ContentEntry {
            r#type: r#type.to_string(),
            path: path.to_string(),
            meta,
        });
    }

    /// 收尾:写入 manifest.toml 并关闭。
    pub fn finish(mut self) -> anyhow::Result<()> {
        let text = toml::to_string_pretty(&self.manifest)?;
        self.writer
            .start_file(MANIFEST_NAME, zip::write::SimpleFileOptions::default())?;
        self.writer.write_all(text.as_bytes())?;
        self.writer.finish()?;
        Ok(())
    }
}

/// 校验归档条目名并返回剥去前缀、**分隔符归一为 `/`** 的相对路径：必须
/// `required_prefix` 前缀、非空，且（`\`归一为`/`后）所有路径段均为普通段——
/// components 白名单，拦 `..`/绝对/盘符相对（`C:foo`）/UNC/`.`，段内禁 `:`
/// （NTFS ADS 防御）。
///
/// ★ **返回归一化后的值，而不是原始切片**。校验按归一化形式做、返回值却保留反斜杠，
/// 会让调用方拿到一个「校验时是这个意思、使用时是另一个意思」的路径：`.contains('/')`
/// 判根在 `sub\evil.schema.toml` 上返回 false，于是子目录文件被当成根方案文件
/// （破坏「根须含 `*.schema.toml`」契约，`schema_ids` 还会提出假 id）；类 Unix 平台上
/// 落盘更会造出文件名里带字面 `\` 的文件。调用方的 join / 判根 / 取 id 一律用本返回值。
pub fn validate_entry_rel(name: &str, required_prefix: &str) -> anyhow::Result<String> {
    let rel = name
        .strip_prefix(required_prefix)
        .ok_or_else(|| anyhow::anyhow!("非法条目(缺 {required_prefix} 前缀): {name}"))?;
    if rel.is_empty() {
        anyhow::bail!("非法条目(空路径): {name}");
    }
    let normalized = rel.replace('\\', "/");
    let ok = Path::new(&normalized).components().all(
        |c| matches!(c, std::path::Component::Normal(seg) if !seg.to_string_lossy().contains(':')),
    );
    if !ok {
        anyhow::bail!("非法条目(路径穿越): {name}");
    }
    if let Some(seg) = normalized.split('/').find(|s| !windows_safe_segment(s)) {
        anyhow::bail!("非法条目(Windows 上无法按原名落盘的文件名「{seg}」): {name}");
    }
    Ok(normalized)
}

/// 路径段能否在 Windows 上按原名落盘：拒绝空段（`a//b`）、尾随点或空格（Win32 会悄悄
/// 截掉，落成另一个名字）、设备保留名（与主题 id 共用
/// [`wind_theme::is_windows_reserved_name`]）。
fn windows_safe_segment(seg: &str) -> bool {
    !seg.is_empty() && !seg.ends_with(['.', ' ']) && !wind_theme::is_windows_reserved_name(seg)
}

/// 免全解压读取并校验 manifest.toml。
pub fn read_manifest(path: &Path) -> anyhow::Result<Manifest> {
    let file = std::fs::File::open(path)?;
    let mut archive = zip::ZipArchive::new(file)?;
    let mut entry = archive
        .by_name(MANIFEST_NAME)
        .map_err(|_| anyhow::anyhow!("归档缺少 {}", MANIFEST_NAME))?;
    let mut buf = String::new();
    entry.read_to_string(&mut buf)?;
    let manifest: Manifest = toml::from_str(&buf)?;
    manifest.validate()?;
    Ok(manifest)
}

/// 读取单个条目字节。
pub fn extract_entry(path: &Path, name: &str) -> anyhow::Result<Vec<u8>> {
    let file = std::fs::File::open(path)?;
    let mut archive = zip::ZipArchive::new(file)?;
    let mut entry = archive
        .by_name(name)
        .map_err(|_| anyhow::anyhow!("归档缺少条目 {}", name))?;
    let mut buf = Vec::new();
    entry.read_to_end(&mut buf)?;
    Ok(buf)
}

/// 读取单个条目字节，**上限 `max`**（超出即报错）。
///
/// ★ 与「先读完再判长度」不是一回事：`Read::take` 让解压在 `max + 1` 字节处停下，
/// 超额部分根本不进内存。zip 的中央目录里那个「解压后大小」是**归档自己声明的**，
/// 一个 1 KB 的条目完全可以声明 1 KB 而解压出 10 GB——按声明值预检只挡得住诚实的
/// 归档，真正的界必须画在读取上。
pub fn extract_entry_limited(path: &Path, name: &str, max: u64) -> anyhow::Result<Vec<u8>> {
    let file = std::fs::File::open(path)?;
    let mut archive = zip::ZipArchive::new(file)?;
    let entry = archive
        .by_name(name)
        .map_err(|_| anyhow::anyhow!("归档缺少条目 {}", name))?;
    let mut buf = Vec::new();
    // 多读 1 字节即可判定「超了」，且这 1 字节是可承受的越界量。
    entry.take(max + 1).read_to_end(&mut buf)?;
    if buf.len() as u64 > max {
        anyhow::bail!("归档条目 {name} 解压后超过上限（{max} 字节）");
    }
    Ok(buf)
}

#[cfg(test)]
#[path = "bundle_tests.rs"]
mod tests;
