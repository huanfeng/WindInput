//! `theme.renderPreview` / `theme.renderPreviews`：主题预览图（服务端真实渲染器出 PNG）+ 磁盘缓存。
//!
//! 出图走协调器的 [`WebDataHost::request_candidate_preview`]（UI 线程用实时候选窗的文字后端与外观
//! 覆盖画），本模块只管：参数、主题来源（已装 / 主题包 / TOML 文本）、缓存键、缓存目录与清理。
//! 协议与阈值见 `docs/design/wind-scene-extraction.md` §7。
//!
//! 缓存键（128 位 SipHash，十六进制）覆盖：渲染器版本、外观指纹（候选窗 / 字体配置）、明暗、
//! 横竖、缩放，以及主题 base 链上每个目录的全部文件（相对路径 + 字节）。按内容而非路径哈希，
//! 所以改主题文本、换资源、改覆盖、升渲染器版本都换键，旧图不会再被命中，由清理收走。

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant, SystemTime};

use serde_json::{Value, json};
use wind_coordinator::web_host::WebDataHost;

use crate::WebDataRpc;

/// 缓存目录名（在 `Config::cache_dir()` 下）。
pub const PREVIEW_CACHE_DIR: &str = "theme_previews";
/// 超过这么久未被访问（命中会刷新 mtime）的预览图在清理时删除。
pub const PREVIEW_MAX_IDLE: Duration = Duration::from_secs(30 * 24 * 3600);
/// 缓存目录总量上限，超出按 mtime 从旧到新淘汰。
pub const PREVIEW_MAX_TOTAL_BYTES: u64 = 50 * 1024 * 1024;
/// 等 UI 线程出图的时限：基数 + 每张未命中加一点，封顶。超时返回错误，不挂死 RPC。
const RENDER_TIMEOUT_BASE: Duration = Duration::from_secs(3);
const RENDER_TIMEOUT_PER_MISS: Duration = Duration::from_secs(1);
const RENDER_TIMEOUT_MAX: Duration = Duration::from_secs(20);
/// 一次批量的条数上限。
const MAX_BATCH: usize = 64;
/// 包 / 文本主题的临时解包目录（`<缓存>/staging/<唯一名>/`），崩溃残留超过这么久清掉。
const STAGING_STALE: Duration = Duration::from_secs(3600);
/// 临时目录里被预览主题的 slug。
const STAGED_SLUG: &str = "_render_preview";

/// 一条预览的主题来源。
enum Source {
    Installed(String),
    Package(PathBuf),
    Text(String),
}

/// 临时解包目录，离开作用域即删。
struct Staging(PathBuf);

impl Drop for Staging {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// 已解析好、待查缓存 / 出图的一条。
struct Prepared {
    /// 缓存文件名前缀：已装主题用 slug（删主题时按它清），其余 `_pkg` / `_text`。
    prefix: String,
    dirs: Vec<PathBuf>,
    slug: String,
    is_dark: bool,
    vertical: bool,
    scale: f32,
    inline: bool,
    _staging: Option<Staging>,
}

/// `theme.renderPreview`：单条，失败即 RPC 错误。
pub(crate) fn render_one<H: WebDataRpc + ?Sized>(h: &H, params: &Value) -> anyhow::Result<Value> {
    let cache = default_cache_dir();
    let mut out = render_batch(h, std::slice::from_ref(params), cache.as_deref());
    let v = out.pop().unwrap_or(Value::Null);
    if v.get("ok").and_then(Value::as_bool) == Some(true) {
        Ok(v)
    } else {
        anyhow::bail!(
            "{}",
            v.get("error")
                .and_then(Value::as_str)
                .unwrap_or("预览渲染失败")
        )
    }
}

/// `theme.renderPreviews { items:[...] }`：逐条回执（`ok` 为假时带 `error`），顺序与请求一致。
pub(crate) fn render_many<H: WebDataRpc + ?Sized>(h: &H, params: &Value) -> anyhow::Result<Value> {
    let items = params
        .get("items")
        .and_then(Value::as_array)
        .ok_or_else(|| anyhow::anyhow!("缺少参数 items"))?;
    if items.len() > MAX_BATCH {
        anyhow::bail!("一次最多 {MAX_BATCH} 条（收到 {}）", items.len());
    }
    let cache = default_cache_dir();
    Ok(json!({ "items": render_batch(h, items, cache.as_deref()) }))
}

/// 删主题时顺带删它的预览图（找不到缓存目录或删失败都不影响删主题本身）。
pub(crate) fn forget_theme(slug: &str) {
    if let Some(dir) = default_cache_dir() {
        remove_theme_previews(&dir, slug);
    }
}

fn default_cache_dir() -> Option<PathBuf> {
    wind_config::Config::cache_dir().map(|d| d.join(PREVIEW_CACHE_DIR))
}

/// 批量本体：先逐条查缓存，未命中的一次全部投给 UI 线程，再按共同时限收回执。
/// `cache` 为 None（取不到本机目录）时不落盘，一律内联 base64。
pub(crate) fn render_batch<H: WebDataHost + WebDataRpc + ?Sized>(
    h: &H,
    items: &[Value],
    cache: Option<&Path>,
) -> Vec<Value> {
    if let Some(dir) = cache {
        prune_once(dir);
    }
    let fingerprint = h.candidate_preview_fingerprint();
    let mut out: Vec<Value> = Vec::with_capacity(items.len());
    // (下标, 回执, 落盘路径, 键, 内联, 临时目录)：临时目录要活到出图完成。
    type Pending = (
        usize,
        wind_coordinator::candidate_preview::PreviewReply,
        Option<PathBuf>,
        String,
        bool,
        Option<Staging>,
    );
    let mut pending: Vec<Pending> = Vec::new();
    for (i, item) in items.iter().enumerate() {
        let p = match prepare(h, item, cache) {
            Ok(p) => p,
            Err(e) => {
                out.push(err_json(&e.to_string()));
                continue;
            }
        };
        let key = match cache_key(
            wind_ui_types::CANDIDATE_PREVIEW_RENDER_VERSION,
            &fingerprint,
            &p,
        ) {
            Ok(k) => k,
            Err(e) => {
                out.push(err_json(&format!("读取主题文件失败：{e}")));
                continue;
            }
        };
        let path = cache.map(|d| d.join(format!("{}.{key}.png", p.prefix)));
        if let Some(path) = &path
            && let Some(v) = cache_hit(path, &key, p.inline)
        {
            out.push(v);
            continue;
        }
        let theme = match wind_theme::load_resolved_dirs(&p.dirs, &p.slug, p.is_dark) {
            Ok(t) => t,
            Err(e) => {
                out.push(err_json(&format!("主题加载失败：{e:#}")));
                continue;
            }
        };
        let rx = h.request_candidate_preview(theme, p.vertical, p.scale);
        out.push(Value::Null);
        pending.push((i, rx, path, key, p.inline, p._staging));
    }
    if pending.is_empty() {
        return out;
    }
    let budget = (RENDER_TIMEOUT_BASE + RENDER_TIMEOUT_PER_MISS * pending.len() as u32)
        .min(RENDER_TIMEOUT_MAX);
    let deadline = Instant::now() + budget;
    let mut wrote = false;
    for (i, rx, path, key, inline, staging) in pending {
        let left = deadline.saturating_duration_since(Instant::now());
        out[i] = match rx.recv_timeout(left) {
            Ok(Ok(img)) => {
                let saved = path.filter(|p| write_atomic(p, &img.png).is_ok());
                wrote |= saved.is_some();
                image_json(
                    &key,
                    img.width,
                    img.height,
                    saved.as_deref(),
                    &img.png,
                    inline,
                    false,
                )
            }
            Ok(Err(e)) => err_json(&e),
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                err_json("预览渲染超时（界面线程忙），请稍后重试")
            }
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                err_json("当前运行形态不支持预览渲染（没有界面线程）")
            }
        };
        drop(staging);
    }
    if wrote && let Some(dir) = cache {
        let _ = prune_cache(
            dir,
            SystemTime::now(),
            PREVIEW_MAX_IDLE,
            PREVIEW_MAX_TOTAL_BYTES,
        );
    }
    out
}

/// 解析一条参数并定位主题目录（包 / 文本先解到临时目录）。
fn prepare<H: WebDataHost + WebDataRpc + ?Sized>(
    h: &H,
    item: &Value,
    cache: Option<&Path>,
) -> anyhow::Result<Prepared> {
    let s = |k: &str| {
        item.get(k)
            .and_then(Value::as_str)
            .filter(|v| !v.is_empty())
    };
    let source = match (s("name"), s("package"), s("text")) {
        (Some(n), None, None) => Source::Installed(n.to_string()),
        (None, Some(p), None) => Source::Package(PathBuf::from(p)),
        (None, None, Some(t)) => Source::Text(t.to_string()),
        (None, None, None) => Source::Installed(h.current_theme_name()),
        _ => anyhow::bail!("name / package / text 只能给一个"),
    };
    let is_dark = match s("style") {
        None => h.current_theme_is_dark(),
        Some("light") => false,
        Some("dark") => true,
        Some("system") => wind_coordinator::theme_style::system_prefers_dark(),
        Some(o) => anyhow::bail!("style 只能是 light / dark / system（收到 {o}）"),
    };
    let vertical = match s("layout") {
        None => h.candidate_preview_vertical(),
        Some("horizontal") => false,
        Some("vertical") => true,
        Some(o) => anyhow::bail!("layout 只能是 horizontal / vertical（收到 {o}）"),
    };
    let scale = match item.get("scale") {
        None | Some(Value::Null) => 1.0,
        Some(v) => v
            .as_f64()
            .filter(|x| (0.5..=4.0).contains(x))
            .ok_or_else(|| anyhow::anyhow!("scale 须是 0.5–4 之间的数"))? as f32,
    };
    let inline = item.get("inline").and_then(Value::as_bool).unwrap_or(false) || cache.is_none();
    let installed = h.theme_dirs();
    let (prefix, slug, dirs, staging) = match source {
        Source::Installed(slug) => {
            // 合法性、定制版隐藏、存在性：与读取侧同一判据。
            h.theme_dir_of(&slug)?;
            (slug.clone(), slug, installed, None)
        }
        Source::Package(pkg) => {
            let st = new_staging(cache)?;
            wind_transfer::theme::import_package(&pkg, &st.0.join(STAGED_SLUG), |_| Ok(()))?;
            let dirs = staged_dirs(&st, installed);
            ("_pkg".to_string(), STAGED_SLUG.to_string(), dirs, Some(st))
        }
        Source::Text(text) => {
            wind_theme::validate_text(&text)?;
            let st = new_staging(cache)?;
            let dir = st.0.join(STAGED_SLUG);
            std::fs::create_dir_all(&dir)?;
            std::fs::write(dir.join(wind_transfer::theme::THEME_ENTRY_NAME), text)?;
            let dirs = staged_dirs(&st, installed);
            ("_text".to_string(), STAGED_SLUG.to_string(), dirs, Some(st))
        }
    };
    Ok(Prepared {
        prefix,
        dirs,
        slug,
        is_dark,
        vertical,
        scale,
        inline,
        _staging: staging,
    })
}

/// 临时目录排在搜索链最前（被预览的主题），其后是已装主题（供 base 继承）。
fn staged_dirs(st: &Staging, installed: Vec<PathBuf>) -> Vec<PathBuf> {
    std::iter::once(st.0.clone()).chain(installed).collect()
}

fn new_staging(cache: Option<&Path>) -> anyhow::Result<Staging> {
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let root = staging_root(cache);
    let nanos = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let dir = root.join(format!(
        "{}-{nanos}-{}",
        std::process::id(),
        SEQ.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::create_dir_all(&dir)?;
    Ok(Staging(dir))
}

fn staging_root(cache: Option<&Path>) -> PathBuf {
    match cache {
        Some(d) => d.join("staging"),
        None => std::env::temp_dir().join("wind_theme_preview_staging"),
    }
}

/// 缓存键。`version` 单独成参只为测试能验「升版本即换键」。
fn cache_key(version: u32, fingerprint: &str, p: &Prepared) -> std::io::Result<String> {
    use std::hash::Hasher;
    let mut h = siphasher::sip128::SipHasher13::new_with_keys(0x5749_4e44_5052_4556, 0x0001);
    h.write_u32(version);
    h.write(fingerprint.as_bytes());
    h.write_u8(0xff);
    h.write_u8(p.is_dark as u8);
    h.write_u8(p.vertical as u8);
    h.write_u32(p.scale.to_bits());
    for dir in wind_theme::theme::theme_chain_dirs(&p.dirs, &p.slug) {
        h.write_u8(0xfe);
        hash_dir(&mut h, &dir)?;
    }
    use siphasher::sip128::Hasher128;
    Ok(format!("{:032x}", h.finish128().as_u128()))
}

/// 目录下全部文件（递归，按相对路径排序）的相对路径与字节喂进哈希。路径用 `/` 分隔，
/// 与目录在哪无关——临时解包目录每次不同，同一个包仍得同一个键。
fn hash_dir(h: &mut impl std::hash::Hasher, dir: &Path) -> std::io::Result<()> {
    let mut files = Vec::new();
    collect_files(dir, dir, &mut files)?;
    files.sort();
    for (rel, abs) in files {
        let bytes = std::fs::read(&abs)?;
        h.write(rel.as_bytes());
        h.write_u8(0);
        h.write_u64(bytes.len() as u64);
        h.write(&bytes);
    }
    Ok(())
}

fn collect_files(root: &Path, dir: &Path, out: &mut Vec<(String, PathBuf)>) -> std::io::Result<()> {
    for e in std::fs::read_dir(dir)? {
        let e = e?;
        let path = e.path();
        if e.file_type()?.is_dir() {
            collect_files(root, &path, out)?;
        } else {
            let rel = path
                .strip_prefix(root)
                .unwrap_or(&path)
                .components()
                .map(|c| c.as_os_str().to_string_lossy())
                .collect::<Vec<_>>()
                .join("/");
            out.push((rel, path));
        }
    }
    Ok(())
}

/// 命中：刷新 mtime（清理按它判闲置），读 PNG 头拿宽高。
fn cache_hit(path: &Path, key: &str, inline: bool) -> Option<Value> {
    let bytes = std::fs::read(path).ok()?;
    let (w, h) = png_size(&bytes)?;
    if let Ok(f) = std::fs::OpenOptions::new().write(true).open(path) {
        let _ = f.set_modified(SystemTime::now());
    }
    Some(image_json(key, w, h, Some(path), &bytes, inline, true))
}

fn image_json(
    key: &str,
    width: u32,
    height: u32,
    path: Option<&Path>,
    png: &[u8],
    inline: bool,
    cached: bool,
) -> Value {
    let mut v = json!({
        "ok": true,
        "hash": key,
        "width": width,
        "height": height,
        "cached": cached,
    });
    if let Some(p) = path {
        v["path"] = json!(p.to_string_lossy());
    }
    if inline || path.is_none() {
        use base64::Engine;
        v["png"] = json!(base64::engine::general_purpose::STANDARD.encode(png));
    }
    v
}

fn err_json(msg: &str) -> Value {
    json!({ "ok": false, "error": msg })
}

/// PNG 头（IHDR）里的宽高。
fn png_size(b: &[u8]) -> Option<(u32, u32)> {
    const SIG: [u8; 8] = [0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a];
    if b.len() < 24 || b[..8] != SIG || &b[12..16] != b"IHDR" {
        return None;
    }
    let w = u32::from_be_bytes(b[16..20].try_into().ok()?);
    let h = u32::from_be_bytes(b[20..24].try_into().ok()?);
    Some((w, h))
}

/// 先写同目录临时文件再改名：并发的两个请求写同一键也不会读到半张图。
fn write_atomic(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    static SEQ: AtomicU64 = AtomicU64::new(0);
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension(format!(
        "tmp-{}-{}",
        std::process::id(),
        SEQ.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::write(&tmp, bytes)?;
    if let Err(e) = std::fs::rename(&tmp, path) {
        let _ = std::fs::remove_file(&tmp);
        // Windows 上目标已存在时改名失败：别人刚写好同一键，内容相同，算成功。
        if !path.is_file() {
            return Err(e);
        }
    }
    Ok(())
}

/// 每个服务进程第一次用到预览时清一次（相当于启动时清理，不拖慢启动）。
fn prune_once(dir: &Path) {
    static DONE: std::sync::Once = std::sync::Once::new();
    DONE.call_once(|| {
        match prune_cache(
            dir,
            SystemTime::now(),
            PREVIEW_MAX_IDLE,
            PREVIEW_MAX_TOTAL_BYTES,
        ) {
            Ok(n) if n > 0 => tracing::info!("主题预览缓存清理：删除 {n} 个文件"),
            _ => {}
        }
    });
}

/// 清理：闲置超过 `max_idle` 的图删掉；余下总量超过 `max_total` 时按 mtime 从旧到新删到不超；
/// 顺带清崩溃残留的临时文件与解包目录。返回删掉的预览图 / 临时文件数。
pub(crate) fn prune_cache(
    dir: &Path,
    now: SystemTime,
    max_idle: Duration,
    max_total: u64,
) -> std::io::Result<usize> {
    let rd = match std::fs::read_dir(dir) {
        Ok(rd) => rd,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(0),
        Err(e) => return Err(e),
    };
    let age = |t: SystemTime| now.duration_since(t).unwrap_or_default();
    let mut removed = 0;
    let mut live: Vec<(SystemTime, u64, PathBuf)> = Vec::new();
    for e in rd.flatten() {
        let path = e.path();
        let Ok(md) = e.metadata() else { continue };
        let mtime = md.modified().unwrap_or(now);
        let name = e.file_name().to_string_lossy().into_owned();
        if md.is_dir() {
            if name == "staging" {
                for s in std::fs::read_dir(&path).into_iter().flatten().flatten() {
                    let m = s
                        .metadata()
                        .ok()
                        .and_then(|m| m.modified().ok())
                        .unwrap_or(now);
                    if age(m) > STAGING_STALE {
                        let _ = std::fs::remove_dir_all(s.path());
                    }
                }
            }
            continue;
        }
        if name.contains(".tmp-") {
            if age(mtime) > STAGING_STALE && std::fs::remove_file(&path).is_ok() {
                removed += 1;
            }
            continue;
        }
        if !name.ends_with(".png") {
            continue;
        }
        if age(mtime) > max_idle {
            if std::fs::remove_file(&path).is_ok() {
                removed += 1;
            }
            continue;
        }
        live.push((mtime, md.len(), path));
    }
    let mut total: u64 = live.iter().map(|(_, n, _)| n).sum();
    if total > max_total {
        live.sort_by_key(|(t, _, _)| *t);
        for (_, n, path) in live {
            if total <= max_total {
                break;
            }
            if std::fs::remove_file(&path).is_ok() {
                total -= n;
                removed += 1;
            }
        }
    }
    Ok(removed)
}

/// 删掉某个已装主题的全部预览图（文件名 `<slug>.<32 位十六进制>.png`，按整段精确匹配）。
pub(crate) fn remove_theme_previews(dir: &Path, slug: &str) -> usize {
    let mut n = 0;
    for e in std::fs::read_dir(dir).into_iter().flatten().flatten() {
        let name = e.file_name().to_string_lossy().into_owned();
        let Some((prefix, key)) = name.strip_suffix(".png").and_then(|s| s.rsplit_once('.')) else {
            continue;
        };
        if prefix == slug
            && key.len() == 32
            && key.bytes().all(|b| b.is_ascii_hexdigit())
            && std::fs::remove_file(e.path()).is_ok()
        {
            n += 1;
        }
    }
    n
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use wind_config::Config;
    use wind_coordinator::Coordinator;
    use wind_ui_types::{CandidatePreviewImage, UiCommand};

    fn tmp(tag: &str) -> PathBuf {
        let d =
            std::env::temp_dir().join(format!("wind_theme_render_{tag}_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn prepared(dirs: Vec<PathBuf>, slug: &str) -> Prepared {
        Prepared {
            prefix: slug.into(),
            dirs,
            slug: slug.into(),
            is_dark: false,
            vertical: false,
            scale: 1.0,
            inline: false,
            _staging: None,
        }
    }

    /// 两层主题：`base_t`（带一张资源）← `child`。
    fn two_themes(root: &Path) {
        let b = root.join("base_t");
        std::fs::create_dir_all(b.join("assets")).unwrap();
        std::fs::write(b.join("theme.toml"), "[meta]\nname = \"b\"\n").unwrap();
        std::fs::write(b.join("assets/bg.png"), b"one").unwrap();
        let c = root.join("child");
        std::fs::create_dir_all(&c).unwrap();
        std::fs::write(
            c.join("theme.toml"),
            "base = \"base_t\"\n[meta]\nname = \"c\"\n",
        )
        .unwrap();
    }

    #[test]
    fn key_covers_text_assets_base_overrides_params_and_version() {
        let root = tmp("key");
        two_themes(&root);
        let dirs = vec![root.clone()];
        let p = prepared(dirs.clone(), "child");
        let k0 = cache_key(1, "fp", &p).unwrap();
        assert_eq!(k0, cache_key(1, "fp", &p).unwrap(), "同输入同键");
        assert_eq!(k0.len(), 32);
        assert_ne!(k0, cache_key(2, "fp", &p).unwrap(), "渲染器版本");
        assert_ne!(k0, cache_key(1, "fp2", &p).unwrap(), "外观覆盖");
        let mut q = prepared(dirs.clone(), "child");
        q.is_dark = true;
        assert_ne!(k0, cache_key(1, "fp", &q).unwrap(), "明暗");
        let mut q = prepared(dirs.clone(), "child");
        q.vertical = true;
        assert_ne!(k0, cache_key(1, "fp", &q).unwrap(), "横竖");
        let mut q = prepared(dirs.clone(), "child");
        q.scale = 1.5;
        assert_ne!(k0, cache_key(1, "fp", &q).unwrap(), "缩放");
        // base 链上的资源字节变了
        std::fs::write(root.join("base_t/assets/bg.png"), b"two").unwrap();
        let k1 = cache_key(1, "fp", &p).unwrap();
        assert_ne!(k0, k1, "base 资源");
        // 主题文本变了
        std::fs::write(
            root.join("child/theme.toml"),
            "base = \"base_t\"\n[meta]\nname = \"c2\"\n",
        )
        .unwrap();
        assert_ne!(k1, cache_key(1, "fp", &p).unwrap(), "主题文本");
        // 搬到别的目录、内容不变：键不变（只哈希相对路径与字节）
        let moved = tmp("key_moved");
        for d in ["base_t", "child"] {
            let to = moved.join(d);
            std::fs::create_dir_all(to.join("assets")).unwrap();
            for f in ["theme.toml", "assets/bg.png"] {
                if let Ok(b) = std::fs::read(root.join(d).join(f)) {
                    std::fs::write(to.join(f), b).unwrap();
                }
            }
        }
        let _ = std::fs::remove_dir(moved.join("child/assets"));
        assert_eq!(
            cache_key(1, "fp", &p).unwrap(),
            cache_key(1, "fp", &prepared(vec![moved.clone()], "child")).unwrap()
        );
        let _ = std::fs::remove_dir_all(root);
        let _ = std::fs::remove_dir_all(moved);
    }

    fn touch(path: &Path, bytes: usize, age: Duration, now: SystemTime) {
        std::fs::write(path, vec![0u8; bytes]).unwrap();
        let f = std::fs::OpenOptions::new().write(true).open(path).unwrap();
        f.set_modified(now - age).unwrap();
    }

    #[test]
    fn prune_drops_idle_then_oldest_over_budget() {
        let dir = tmp("prune");
        let now = SystemTime::now();
        let day = Duration::from_secs(86400);
        touch(&dir.join("idle.png"), 10, day * 40, now);
        touch(&dir.join("old.png"), 60, day * 3, now);
        touch(&dir.join("mid.png"), 60, day * 2, now);
        touch(&dir.join("new.png"), 60, day, now);
        touch(&dir.join("x.png.tmp-1-0"), 5, day, now);
        touch(&dir.join("keep.txt"), 500, day * 90, now);
        let n = prune_cache(&dir, now, day * 30, 130).unwrap();
        assert_eq!(n, 3, "闲置 1 + 超量淘汰最旧 1 + 残留临时文件 1");
        assert!(!dir.join("idle.png").exists());
        assert!(!dir.join("old.png").exists());
        assert!(dir.join("mid.png").exists() && dir.join("new.png").exists());
        assert!(dir.join("keep.txt").exists(), "不是预览图的文件不碰");
        assert_eq!(prune_cache(&dir.join("absent"), now, day, 1).unwrap(), 0);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn remove_theme_previews_matches_slug_exactly() {
        let dir = tmp("forget");
        let key = "0123456789abcdef0123456789abcdef";
        for n in [
            format!("a.{key}.png"),
            format!("a.b.{key}.png"),
            format!("ab.{key}.png"),
            "a.short.png".to_string(),
        ] {
            std::fs::write(dir.join(n), b"x").unwrap();
        }
        assert_eq!(remove_theme_previews(&dir, "a"), 1);
        assert!(dir.join(format!("a.b.{key}.png")).exists());
        assert!(dir.join(format!("ab.{key}.png")).exists());
        assert!(dir.join("a.short.png").exists());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn png_size_reads_ihdr() {
        let mut b = vec![0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a, 0, 0, 0, 13];
        b.extend_from_slice(b"IHDR");
        b.extend_from_slice(&300u32.to_be_bytes());
        b.extend_from_slice(&40u32.to_be_bytes());
        assert_eq!(png_size(&b), Some((300, 40)));
        assert_eq!(png_size(b"not a png at all, no....."), None);
    }

    /// 一张 2×1 的最小合法 PNG 头（只用到宽高）。
    fn fake_png(w: u32, h: u32) -> Vec<u8> {
        let mut b = vec![0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a, 0, 0, 0, 13];
        b.extend_from_slice(b"IHDR");
        b.extend_from_slice(&w.to_be_bytes());
        b.extend_from_slice(&h.to_be_bytes());
        b.extend_from_slice(&[8, 6, 0, 0, 0]);
        b
    }

    /// 真实主题目录 + 假 UI 线程：每个预览请求回一张宽 = 序号的假图，并计数。
    fn coord_with_fake_ui() -> (Arc<Coordinator>, Arc<AtomicU64>) {
        let data = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../data");
        let (c, rx) = Coordinator::new_headless_with_ui(Config::default(), Some(&data));
        let seen = Arc::new(AtomicU64::new(0));
        let s = Arc::clone(&seen);
        std::thread::spawn(move || {
            while let Ok(cmd) = rx.recv() {
                if let UiCommand::RenderCandidatePreview(req) = cmd {
                    let n = s.fetch_add(1, Ordering::SeqCst) as u32 + 1;
                    let w = if req.vertical { 7 } else { 100 + n };
                    let _ = req.reply.try_send(Ok(CandidatePreviewImage {
                        width: w,
                        height: 30,
                        png: fake_png(w, 30),
                    }));
                }
            }
        });
        (c, seen)
    }

    fn ok(v: &Value) -> bool {
        v["ok"].as_bool() == Some(true)
    }

    #[test]
    fn batch_renders_misses_once_then_hits_cache() {
        let (c, seen) = coord_with_fake_ui();
        let cache = tmp("batch");
        let items = [
            json!({ "name": "default", "style": "light" }),
            json!({ "name": "default", "style": "dark" }),
            json!({ "name": "default", "layout": "vertical", "inline": true }),
            json!({ "name": "no-such-theme" }),
            json!({ "name": "default", "scale": 9 }),
            json!({ "name": "default", "text": "x" }),
        ];
        let first = render_batch(c.as_ref(), &items, Some(&cache));
        assert_eq!(first.len(), items.len());
        assert!(ok(&first[0]) && ok(&first[1]) && ok(&first[2]), "{first:?}");
        assert!(
            !ok(&first[3]) && !ok(&first[4]) && !ok(&first[5]),
            "{first:?}"
        );
        assert_eq!(seen.load(Ordering::SeqCst), 3, "三条未命中各渲染一次");
        assert_ne!(first[0]["hash"], first[1]["hash"]);
        assert_eq!(first[2]["width"], 7);
        assert!(first[2]["png"].is_string(), "inline 要带 base64");
        assert!(first[0].get("png").is_none());
        let path = PathBuf::from(first[0]["path"].as_str().unwrap());
        assert!(path.starts_with(&cache) && path.is_file());
        assert!(
            path.file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with("default.")
        );
        assert_eq!(first[0]["cached"], false);

        let again = render_batch(c.as_ref(), &items[..3], Some(&cache));
        assert_eq!(seen.load(Ordering::SeqCst), 3, "全部命中，不再请求 UI");
        for (a, b) in first.iter().zip(&again) {
            assert_eq!(a["hash"], b["hash"]);
            assert_eq!(a["width"], b["width"], "宽高从 PNG 头读回");
            assert_eq!(b["cached"], true);
        }
        assert_eq!(remove_theme_previews(&cache, "default"), 3);
        let _ = std::fs::remove_dir_all(cache);
    }

    #[test]
    fn package_and_text_sources_render_and_clean_staging() {
        let (c, seen) = coord_with_fake_ui();
        let cache = tmp("pkg");
        let data = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../data/themes");
        let pkg = cache.join("jade.wtheme");
        wind_transfer::theme::export_package(&data.join("jade"), &pkg, None).unwrap();
        let text = std::fs::read_to_string(data.join("jade/theme.toml")).unwrap();
        let items = [
            json!({ "package": pkg.to_string_lossy() }),
            json!({ "text": text }),
            json!({ "text": "this is = = not toml" }),
            json!({ "package": cache.join("missing.wtheme").to_string_lossy() }),
        ];
        let out = render_batch(c.as_ref(), &items, Some(&cache));
        assert!(ok(&out[0]) && ok(&out[1]), "{out:?}");
        assert!(!ok(&out[2]) && !ok(&out[3]), "{out:?}");
        assert!(
            out[0]["path"]
                .as_str()
                .unwrap()
                .contains(&format!("{}_pkg.", std::path::MAIN_SEPARATOR))
        );
        assert_eq!(seen.load(Ordering::SeqCst), 2);
        // 同一个包再来一次：解到另一个临时目录，键相同，命中。
        let again = render_batch(c.as_ref(), &items[..1], Some(&cache));
        assert_eq!(again[0]["hash"], out[0]["hash"]);
        assert_eq!(again[0]["cached"], true);
        assert_eq!(seen.load(Ordering::SeqCst), 2);
        let staging: Vec<_> = std::fs::read_dir(cache.join("staging"))
            .map(|r| r.flatten().collect())
            .unwrap_or_default();
        assert!(staging.is_empty(), "临时解包目录用完即删");
        let _ = std::fs::remove_dir_all(cache);
    }

    #[test]
    fn dispatch_routes_and_validates_batch_shape() {
        use crate::WebDataRpc;
        // 只走参数校验失败的分支：成功路径会落到真实缓存目录，由 render_batch 的用例覆盖。
        let c = Coordinator::new_headless(Config::default(), None);
        let e = c
            .web_data_rpc("theme.renderPreviews", &json!({}))
            .unwrap_err();
        assert!(e.to_string().contains("items"), "{e}");
        let many: Vec<Value> = (0..=MAX_BATCH).map(|_| json!({})).collect();
        let e = c
            .web_data_rpc("theme.renderPreviews", &json!({ "items": many }))
            .unwrap_err();
        assert!(e.to_string().contains("最多"), "{e}");
    }

    #[test]
    fn without_ui_thread_reports_error_instead_of_hanging() {
        let data = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../data");
        let c = Coordinator::new_headless(Config::default(), Some(&data));
        let cache = tmp("noui");
        let t = Instant::now();
        let out = render_batch(c.as_ref(), &[json!({ "name": "default" })], Some(&cache));
        assert!(!ok(&out[0]));
        assert!(t.elapsed() < Duration::from_secs(2), "通道断开应立即返回");
        let _ = std::fs::remove_dir_all(cache);
    }
}
