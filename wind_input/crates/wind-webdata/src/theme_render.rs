//! `theme.renderPreview` / `theme.renderPreviews`：主题预览图（服务端真实渲染器出 PNG）+ 磁盘缓存。
//!
//! 出图走协调器的 [`wind_coordinator::web_host::WebDataHost::request_candidate_preview`]（UI 线程用实时候选窗的文字后端与外观
//! 覆盖画），本模块只管：参数、主题来源（已装 / 主题包 / TOML 文本）、缓存键、缓存目录与清理、
//! 在途上限。协议与阈值见 `docs/design/wind-scene-extraction.md` §7。
//!
//! 缓存键（128 位 SipHash，十六进制）覆盖：渲染器版本常量、crate 版本与构建标识
//! （[`set_preview_build_id`]）、外观指纹、明暗、横竖、缩放，以及主题 base 链上每个目录的文件
//! 指纹——已装主题取（相对路径、大小、mtime），临时解包的包 / 文本取内容。

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::time::{Duration, Instant, SystemTime};

use serde_json::{Value, json};

use crate::WebDataRpc;

/// 缓存目录名（在 `Config::cache_dir()` 下）。
pub const PREVIEW_CACHE_DIR: &str = "theme_previews";
/// 超过这么久未被访问（命中会刷新 mtime）的预览图在清理时删除。
pub const PREVIEW_MAX_IDLE: Duration = Duration::from_secs(30 * 24 * 3600);
/// 缓存目录总量上限，超出按 mtime 从旧到新淘汰。
pub const PREVIEW_MAX_TOTAL_BYTES: u64 = 50 * 1024 * 1024;
/// 全进程同时交给 UI 线程、还没收到回执的预览上限。批量内按波次投递；别的请求占满时回「忙」。
pub const MAX_IN_FLIGHT: usize = 8;
/// 一次批量的条数上限。
const MAX_BATCH: usize = 64;
/// `text` 参数的上限，与包内 `theme.toml` 同。
const MAX_TEXT_BYTES: usize = 4 * 1024 * 1024;
/// 算缓存键时最多读这么多字节的主题内容（只有临时解包的才读内容）；超出就不缓存，直接渲染。
const MAX_HASH_BYTES: u64 = 64 * 1024 * 1024;
/// 主题目录群最多这么多个文件参与指纹；超出同样不缓存。
const MAX_HASH_FILES: usize = 4096;
/// 包 / 文本主题的临时解包目录（`<缓存>/staging/<唯一名>/`），崩溃残留超过这么久清掉。
const STAGING_STALE: Duration = Duration::from_secs(3600);
/// 临时目录里被预览主题的 slug。
const STAGED_SLUG: &str = "_render_preview";

/// 等 UI 线程出图的时限：基数 + 每张未命中加一点，封顶。超时返回错误，不挂死 RPC。
#[derive(Clone, Copy)]
struct Timeouts {
    base: Duration,
    per_miss: Duration,
    max: Duration,
}

const TIMEOUTS: Timeouts = Timeouts {
    base: Duration::from_secs(3),
    per_miss: Duration::from_secs(1),
    max: Duration::from_secs(20),
};

static BUILD_ID: std::sync::OnceLock<String> = std::sync::OnceLock::new();
static IN_FLIGHT: AtomicUsize = AtomicUsize::new(0);

/// 宿主注入构建标识（服务进程启动时调一次：git 短哈希，dirty 构建再带构建时刻）。它进缓存键，
/// 换一次构建旧图就不再命中——不靠人记得递增渲染器版本。没注入时只有 crate 版本。
pub fn set_preview_build_id(id: impl Into<String>) {
    let _ = BUILD_ID.set(id.into());
}

fn build_stamp() -> String {
    format!(
        "{}+{}",
        env!("CARGO_PKG_VERSION"),
        BUILD_ID.get().map_or("", String::as_str)
    )
}

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

/// 一个在途名额（[`MAX_IN_FLIGHT`]），离开作用域即还。
struct Slot;

impl Slot {
    fn acquire() -> Option<Slot> {
        let mut n = IN_FLIGHT.load(Ordering::Acquire);
        while n < MAX_IN_FLIGHT {
            match IN_FLIGHT.compare_exchange_weak(n, n + 1, Ordering::AcqRel, Ordering::Acquire) {
                Ok(_) => return Some(Slot),
                Err(cur) => n = cur,
            }
        }
        None
    }
}

impl Drop for Slot {
    fn drop(&mut self) {
        IN_FLIGHT.fetch_sub(1, Ordering::AcqRel);
    }
}

/// 已解析好、待查缓存 / 出图的一条。
struct Prepared {
    /// 缓存文件名前缀（[`file_prefix`]）：已装主题取目录名，其余 `_pkg` / `_text`。
    prefix: String,
    dirs: Vec<PathBuf>,
    slug: String,
    /// 主题在临时目录里（包 / 文本）：指纹取内容；否则取文件元数据。
    staged: bool,
    is_dark: bool,
    vertical: bool,
    scale: f32,
    inline: bool,
    staging: Option<Staging>,
}

/// 已投给 UI 线程、等回执的一条。drop 时置位取消（UI 线程排到它就丢掉），并还名额、删临时目录。
struct Pending {
    index: usize,
    rx: wind_coordinator::candidate_preview::PreviewReply,
    path: Option<PathBuf>,
    key: String,
    inline: bool,
    cancel: Arc<AtomicBool>,
    _slot: Slot,
    _staging: Option<Staging>,
}

impl Drop for Pending {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::Release);
    }
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

/// 批量本体，见 [`render_batch_with`]。
pub(crate) fn render_batch<H: WebDataRpc + ?Sized>(
    h: &H,
    items: &[Value],
    cache: Option<&Path>,
) -> Vec<Value> {
    render_batch_with(h, items, cache, TIMEOUTS)
}

/// 先逐条查缓存；未命中的按波次投给 UI 线程（同时在途不超过 [`MAX_IN_FLIGHT`]，名额被别的请求
/// 占满时该条回「忙」），按共同时限收回执。`cache` 为 None（取不到本机目录）时不落盘，一律内联。
fn render_batch_with<H: WebDataRpc + ?Sized>(
    h: &H,
    items: &[Value],
    cache: Option<&Path>,
    timeouts: Timeouts,
) -> Vec<Value> {
    if let Some(dir) = cache {
        prune_once(dir);
    }
    let fingerprint = h.candidate_preview_fingerprint();
    let stamp = build_stamp();
    let mut out = vec![Value::Null; items.len()];
    let mut misses: VecDeque<(usize, Prepared, Option<String>)> = VecDeque::new();
    for (i, item) in items.iter().enumerate() {
        let p = match prepare(h, item, cache) {
            Ok(p) => p,
            Err(e) => {
                out[i] = err_json(&format!("{e:#}"));
                continue;
            }
        };
        let key = match cache_key(
            wind_ui_types::CANDIDATE_PREVIEW_RENDER_VERSION,
            &stamp,
            &fingerprint,
            &p,
        ) {
            Ok(k) => k,
            Err(e) => {
                out[i] = err_json(&format!("读取主题文件失败：{e}"));
                continue;
            }
        };
        let path = key
            .as_ref()
            .zip(cache)
            .map(|(k, d)| d.join(format!("{}.{k}.png", p.prefix)));
        if let (Some(path), Some(k)) = (&path, &key)
            && let Some(v) = cache_hit(path, k, p.inline)
        {
            out[i] = v;
            continue;
        }
        misses.push_back((i, p, key));
    }
    if misses.is_empty() {
        return out;
    }
    let budget = (timeouts.base + timeouts.per_miss * misses.len() as u32).min(timeouts.max);
    let deadline = Instant::now() + budget;
    let mut in_flight: VecDeque<Pending> = VecDeque::new();
    let mut written: Vec<PathBuf> = Vec::new();
    while let Some((i, p, key)) = misses.pop_front() {
        // 名额：先等自己批里最早的那张收完；自己一张也没有、仍拿不到，就是别的请求占满了。
        let slot = loop {
            if let Some(s) = Slot::acquire() {
                break Some(s);
            }
            match in_flight.pop_front() {
                Some(done) => finish(done, deadline, &mut out, &mut written),
                None => break None,
            }
        };
        let Some(slot) = slot else {
            out[i] = err_json("预览渲染忙，请稍后重试");
            continue;
        };
        let theme = match wind_theme::load_resolved_dirs(&p.dirs, &p.slug, p.is_dark) {
            Ok(t) => t,
            Err(e) => {
                out[i] = err_json(&format!("主题加载失败：{e:#}"));
                continue;
            }
        };
        let cancel = Arc::new(AtomicBool::new(false));
        let rx = h.request_candidate_preview(theme, p.vertical, p.scale, Arc::clone(&cancel));
        let path = key
            .as_ref()
            .zip(cache)
            .map(|(k, d)| d.join(format!("{}.{k}.png", p.prefix)));
        in_flight.push_back(Pending {
            index: i,
            rx,
            path,
            key: key.unwrap_or_default(),
            // 不缓存（键算不出）时只能内联。
            inline: p.inline || cache.is_none(),
            cancel,
            _slot: slot,
            _staging: p.staging,
        });
    }
    while let Some(done) = in_flight.pop_front() {
        finish(done, deadline, &mut out, &mut written);
    }
    if !written.is_empty()
        && let Some(dir) = cache
    {
        let _ = prune_cache(
            dir,
            SystemTime::now(),
            PREVIEW_MAX_IDLE,
            PREVIEW_MAX_TOTAL_BYTES,
            &written,
        );
    }
    out
}

/// 收一条回执写进 `out`；落盘成功的路径记进 `written`。`p` 在此 drop：置位取消、还名额、删临时目录。
fn finish(p: Pending, deadline: Instant, out: &mut [Value], written: &mut Vec<PathBuf>) {
    let left = deadline.saturating_duration_since(Instant::now());
    out[p.index] = match p.rx.recv_timeout(left) {
        Ok(Ok(img)) => {
            let saved = p
                .path
                .clone()
                .filter(|path| write_atomic(path, &img.png).is_ok());
            if let Some(s) = &saved {
                written.push(s.clone());
            }
            let inline = p.inline || saved.is_none();
            image_json(
                &p.key,
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
}

/// 解析一条参数并定位主题目录（包 / 文本先解到临时目录）。
fn prepare<H: WebDataRpc + ?Sized>(
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
            // 合法性、定制版隐藏、存在性：与读取侧同一判据。前缀取实际命中的目录名，不取请求原文。
            let dir = h.theme_dir_of(&slug)?;
            let name = dir
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            (file_prefix(&name), slug, installed, None)
        }
        Source::Package(pkg) => {
            let st = new_staging(cache)?;
            wind_transfer::theme::import_package(&pkg, &st.0.join(STAGED_SLUG), |_| Ok(()))?;
            let dirs = staged_dirs(&st, installed);
            ("_pkg".to_string(), STAGED_SLUG.to_string(), dirs, Some(st))
        }
        Source::Text(text) => {
            if text.len() > MAX_TEXT_BYTES {
                anyhow::bail!("主题文本过大（{} 字节，上限 {MAX_TEXT_BYTES}）", text.len());
            }
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
        staged: staging.is_some(),
        is_dark,
        vertical,
        scale,
        inline,
        staging,
    })
}

/// 主题目录名 → 缓存文件名前缀：只留 `[A-Za-z0-9._-]`；有字符被换掉（如中文目录名）时再拼原名的
/// 短哈希，免得两个主题撞成同一个前缀。
fn file_prefix(name: &str) -> String {
    let ok = |c: char| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-');
    let safe: String = name.chars().map(|c| if ok(c) { c } else { '_' }).collect();
    if !safe.is_empty() && name.chars().all(ok) {
        return safe;
    }
    use std::hash::Hasher;
    let mut h = siphasher::sip::SipHasher13::new_with_keys(0x5749_4e44, 0x0002);
    h.write(name.as_bytes());
    format!("{safe}_{:08x}", h.finish() as u32)
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

/// 缓存键；`Ok(None)` = 主题太大（文件数 / 字节超限），不缓存。`version` / `stamp` 单独成参
/// 只为测试能验「升版本、换构建即换键」。
fn cache_key(
    version: u32,
    stamp: &str,
    fingerprint: &str,
    p: &Prepared,
) -> std::io::Result<Option<String>> {
    use std::hash::Hasher;
    let mut h = siphasher::sip128::SipHasher13::new_with_keys(0x5749_4e44_5052_4556, 0x0001);
    h.write_u32(version);
    h.write(stamp.as_bytes());
    h.write_u8(0xff);
    h.write(fingerprint.as_bytes());
    h.write_u8(0xff);
    h.write_u8(p.is_dark as u8);
    h.write_u8(p.vertical as u8);
    h.write_u32(p.scale.to_bits());
    let mut budget = HashBudget {
        files: MAX_HASH_FILES,
        bytes: MAX_HASH_BYTES,
    };
    for dir in wind_theme::theme::theme_chain_dirs(&p.dirs, &p.slug) {
        h.write_u8(0xfe);
        if !hash_dir(&mut h, &dir, p.staged, &mut budget)? {
            return Ok(None);
        }
    }
    use siphasher::sip128::Hasher128;
    Ok(Some(format!("{:032x}", h.finish128().as_u128())))
}

struct HashBudget {
    files: usize,
    bytes: u64,
}

/// 目录下全部文件（递归，按相对路径排序）的指纹喂进哈希；返回是否在预算内。
///
/// `content` 为真（临时解包目录）喂相对路径 + 字节；否则喂相对路径 + 大小 + mtime——已装主题
/// 每次画廊刷新都要算一遍，读全部字节太贵，改动必然动 mtime。路径用 `/` 分隔，与目录在哪无关
/// （临时目录每次不同，同一个包仍得同一个键）。
fn hash_dir(
    h: &mut impl std::hash::Hasher,
    dir: &Path,
    content: bool,
    budget: &mut HashBudget,
) -> std::io::Result<bool> {
    let mut files = Vec::new();
    if !collect_files(dir, dir, &mut files, budget)? {
        return Ok(false);
    }
    files.sort();
    for (rel, abs) in files {
        let md = std::fs::metadata(&abs)?;
        h.write(rel.as_bytes());
        h.write_u8(0);
        h.write_u64(md.len());
        if content {
            if md.len() > budget.bytes {
                return Ok(false);
            }
            budget.bytes -= md.len();
            h.write(&std::fs::read(&abs)?);
        } else {
            let mtime = md
                .modified()
                .ok()
                .and_then(|t| t.duration_since(SystemTime::UNIX_EPOCH).ok())
                .map_or(0, |d| d.as_nanos());
            h.write_u128(mtime);
        }
    }
    Ok(true)
}

/// 收集目录下的文件。符号链接：指向文件的跟随（按链接目标取元数据与内容），指向目录的跳过
/// （不进链接目录，免得绕回自身）。超出文件数预算返回 false。
fn collect_files(
    root: &Path,
    dir: &Path,
    out: &mut Vec<(String, PathBuf)>,
    budget: &mut HashBudget,
) -> std::io::Result<bool> {
    for e in std::fs::read_dir(dir)? {
        let e = e?;
        let path = e.path();
        let ft = e.file_type()?;
        let is_dir = if ft.is_symlink() {
            match std::fs::metadata(&path) {
                Ok(md) if md.is_dir() => continue,
                Ok(_) => false,
                // 断链：不参与指纹（主题加载同样读不到它）。
                Err(_) => continue,
            }
        } else {
            ft.is_dir()
        };
        if is_dir {
            if !collect_files(root, &path, out, budget)? {
                return Ok(false);
            }
            continue;
        }
        if budget.files == 0 {
            return Ok(false);
        }
        budget.files -= 1;
        let rel = path
            .strip_prefix(root)
            .unwrap_or(&path)
            .components()
            .map(|c| c.as_os_str().to_string_lossy())
            .collect::<Vec<_>>()
            .join("/");
        out.push((rel, path));
    }
    Ok(true)
}

/// 命中：校验是一张完整的 PNG（头 + 结尾 IEND），坏了删掉当未命中；好的刷新 mtime（清理按它判
/// 闲置），读 PNG 头拿宽高。
fn cache_hit(path: &Path, key: &str, inline: bool) -> Option<Value> {
    let bytes = std::fs::read(path).ok()?;
    let Some((w, h)) = png_size(&bytes).filter(|_| png_complete(&bytes)) else {
        let _ = std::fs::remove_file(path);
        return None;
    };
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

/// 以 IEND 块（长度 0 + `IEND` + 固定 CRC）收尾：写到一半的文件没有它。
fn png_complete(b: &[u8]) -> bool {
    const IEND: [u8; 12] = [0, 0, 0, 0, b'I', b'E', b'N', b'D', 0xae, 0x42, 0x60, 0x82];
    b.len() >= 8 + 25 + IEND.len() && b.ends_with(&IEND)
}

/// 先写同目录临时文件（`<前缀>.<键>.tmp-<pid>-<序号>`）再改名：并发的两个请求写同一键也不会读到
/// 半张图。任何一步失败都删掉临时文件。
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
    if let Err(e) = std::fs::write(&tmp, bytes) {
        let _ = std::fs::remove_file(&tmp);
        return Err(e);
    }
    if let Err(e) = std::fs::rename(&tmp, path) {
        let _ = std::fs::remove_file(&tmp);
        // 改名会覆盖已有的目标；失败多半是目标正被别的读者占着（Windows 共享冲突）。
        // 那份是另一个请求刚写好的同一键，内容相同，算成功。
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
            &[],
        ) {
            Ok(n) if n > 0 => tracing::info!("主题预览缓存清理：删除 {n} 个文件"),
            _ => {}
        }
    });
}

/// 拆缓存文件名：`<前缀>.<32 位十六进制键>.png` → `(前缀, 键)`。
fn split_preview_name(name: &str) -> Option<(&str, &str)> {
    let (prefix, key) = name.strip_suffix(".png")?.rsplit_once('.')?;
    is_key(key).then_some((prefix, key))
}

/// 本模块写的临时文件：`<前缀>.<键>.tmp-<pid>-<序号>`。
fn is_preview_tmp(name: &str) -> bool {
    let Some((stem, tail)) = name.rsplit_once(".tmp-") else {
        return false;
    };
    let digits = |s: &str| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit());
    let tail_ok = tail
        .split_once('-')
        .is_some_and(|(pid, seq)| digits(pid) && digits(seq));
    tail_ok && stem.rsplit_once('.').is_some_and(|(_, key)| is_key(key))
}

fn is_key(s: &str) -> bool {
    s.len() == 32 && s.bytes().all(|b| b.is_ascii_hexdigit())
}

/// 清理：闲置超过 `max_idle` 的图删掉；余下总量超过 `max_total` 时按 mtime 从旧到新删到不超；
/// 顺带清崩溃残留的临时文件与解包目录（超过 [`STAGING_STALE`]）。`keep` 里的路径（本批刚写的）
/// 不删也不计入淘汰候选。只认本模块的文件名形态，别的文件不碰。返回删掉的文件数。
pub(crate) fn prune_cache(
    dir: &Path,
    now: SystemTime,
    max_idle: Duration,
    max_total: u64,
    keep: &[PathBuf],
) -> std::io::Result<usize> {
    let rd = match std::fs::read_dir(dir) {
        Ok(rd) => rd,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(0),
        Err(e) => return Err(e),
    };
    let age = |t: SystemTime| now.duration_since(t).unwrap_or_default();
    let mut removed = 0;
    let mut kept_bytes = 0u64;
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
                    if age(m) > STAGING_STALE && std::fs::remove_dir_all(s.path()).is_ok() {
                        removed += 1;
                    }
                }
            }
            continue;
        }
        if is_preview_tmp(&name) {
            if age(mtime) > STAGING_STALE && std::fs::remove_file(&path).is_ok() {
                removed += 1;
            }
            continue;
        }
        if split_preview_name(&name).is_none() {
            continue;
        }
        if keep.contains(&path) {
            kept_bytes += md.len();
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
    let mut total: u64 = kept_bytes + live.iter().map(|(_, n, _)| n).sum::<u64>();
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

/// 删掉某个已装主题的全部预览图（按 [`file_prefix`] 整段精确匹配前缀）。
pub(crate) fn remove_theme_previews(dir: &Path, slug: &str) -> usize {
    let want = file_prefix(slug);
    let mut n = 0;
    for e in std::fs::read_dir(dir).into_iter().flatten().flatten() {
        let name = e.file_name().to_string_lossy().into_owned();
        if split_preview_name(&name).is_some_and(|(prefix, _)| prefix == want)
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
    use wind_config::Config;
    use wind_coordinator::Coordinator;
    use wind_ui_types::{CandidatePreviewImage, CandidatePreviewRequest, UiCommand};

    /// 在途名额是全进程的：动到它的用例串行跑，免得互相挤占出「忙」。
    static SERIAL: std::sync::Mutex<()> = std::sync::Mutex::new(());

    fn serial() -> std::sync::MutexGuard<'static, ()> {
        SERIAL.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn tmp(tag: &str) -> PathBuf {
        let d =
            std::env::temp_dir().join(format!("wind_theme_render_{tag}_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn prepared(dirs: Vec<PathBuf>, slug: &str, staged: bool) -> Prepared {
        Prepared {
            prefix: slug.into(),
            dirs,
            slug: slug.into(),
            staged,
            is_dark: false,
            vertical: false,
            scale: 1.0,
            inline: false,
            staging: None,
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

    fn key(v: u32, stamp: &str, fp: &str, p: &Prepared) -> String {
        cache_key(v, stamp, fp, p).unwrap().unwrap()
    }

    #[test]
    fn key_covers_text_assets_base_overrides_params_and_build() {
        for staged in [true, false] {
            let root = tmp(&format!("key{staged}"));
            two_themes(&root);
            let dirs = vec![root.clone()];
            let p = prepared(dirs.clone(), "child", staged);
            let k0 = key(1, "b", "fp", &p);
            assert_eq!(k0, key(1, "b", "fp", &p), "同输入同键");
            assert_eq!(k0.len(), 32);
            assert_ne!(k0, key(2, "b", "fp", &p), "渲染器版本");
            assert_ne!(k0, key(1, "b2", "fp", &p), "构建标识");
            assert_ne!(k0, key(1, "b", "fp2", &p), "外观覆盖");
            let mut q = prepared(dirs.clone(), "child", staged);
            q.is_dark = true;
            assert_ne!(k0, key(1, "b", "fp", &q), "明暗");
            let mut q = prepared(dirs.clone(), "child", staged);
            q.vertical = true;
            assert_ne!(k0, key(1, "b", "fp", &q), "横竖");
            let mut q = prepared(dirs.clone(), "child", staged);
            q.scale = 1.5;
            assert_ne!(k0, key(1, "b", "fp", &q), "缩放");
            // base 链上的资源变了（同长度内容 + 推后 mtime，元数据指纹也看得见）
            let bg = root.join("base_t/assets/bg.png");
            std::fs::write(&bg, b"two").unwrap();
            let f = std::fs::OpenOptions::new().write(true).open(&bg).unwrap();
            f.set_modified(SystemTime::now() + Duration::from_secs(5))
                .unwrap();
            let k1 = key(1, "b", "fp", &p);
            assert_ne!(k0, k1, "base 资源（staged={staged}）");
            std::fs::write(
                root.join("child/theme.toml"),
                "base = \"base_t\"\n[meta]\nname = \"c2\"\n",
            )
            .unwrap();
            assert_ne!(k1, key(1, "b", "fp", &p), "主题文本（staged={staged}）");
            let _ = std::fs::remove_dir_all(root);
        }
    }

    #[test]
    fn staged_key_is_location_independent_and_bounded() {
        let a = tmp("loc_a");
        let b = tmp("loc_b");
        two_themes(&a);
        two_themes(&b);
        assert_eq!(
            key(1, "", "fp", &prepared(vec![a.clone()], "child", true)),
            key(1, "", "fp", &prepared(vec![b.clone()], "child", true)),
            "内容指纹只看相对路径与字节"
        );
        // 目录符号链接被跳过，不当文件读、不报错。
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(&a, a.join("child/loop")).unwrap();
            assert!(cache_key(1, "", "fp", &prepared(vec![a.clone()], "child", true)).is_ok());
        }
        // 文件数超预算：不缓存。
        let mut budget = HashBudget {
            files: 1,
            bytes: MAX_HASH_BYTES,
        };
        let mut h = siphasher::sip::SipHasher13::new();
        assert!(!hash_dir(&mut h, &a.join("base_t"), true, &mut budget).unwrap());
        let _ = std::fs::remove_dir_all(a);
        let _ = std::fs::remove_dir_all(b);
    }

    fn touch(path: &Path, bytes: usize, age: Duration, now: SystemTime) {
        std::fs::write(path, vec![0u8; bytes]).unwrap();
        let f = std::fs::OpenOptions::new().write(true).open(path).unwrap();
        f.set_modified(now - age).unwrap();
    }

    const K: &str = "0123456789abcdef0123456789abcdef";

    #[test]
    fn prune_drops_idle_then_oldest_over_budget() {
        let dir = tmp("prune");
        let now = SystemTime::now();
        let day = Duration::from_secs(86400);
        let f = |n: &str| dir.join(format!("{n}.{K}.png"));
        touch(&f("idle"), 10, day * 40, now);
        touch(&f("old"), 60, day * 3, now);
        touch(&f("mid"), 60, day * 2, now);
        touch(&f("new"), 60, day, now);
        touch(&f("fresh"), 60, day * 5, now);
        touch(&dir.join(format!("x.{K}.tmp-1-0")), 5, day, now);
        touch(&dir.join("x.png.tmp-1-0"), 5, day, now);
        touch(&dir.join("keep.txt"), 500, day * 90, now);
        touch(&dir.join("other.png"), 500, day * 90, now);
        // fresh 是「本批刚写的」：哪怕 mtime 最旧也不删，但算进总量。
        let n = prune_cache(&dir, now, day * 30, 130, &[f("fresh")]).unwrap();
        assert_eq!(n, 4, "闲置 1 + 超量淘汰最旧 2 + 残留临时文件 1");
        assert!(!f("idle").exists() && !f("old").exists() && !f("mid").exists());
        assert!(f("new").exists() && f("fresh").exists());
        assert!(dir.join("x.png.tmp-1-0").exists(), "形态不对的临时文件不碰");
        assert!(dir.join("keep.txt").exists() && dir.join("other.png").exists());
        assert_eq!(
            prune_cache(&dir.join("absent"), now, day, 1, &[]).unwrap(),
            0
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn prune_removes_stale_staging_dirs() {
        let dir = tmp("prune_staging");
        let st = dir.join("staging/123-0-0/_render_preview");
        std::fs::create_dir_all(&st).unwrap();
        std::fs::write(st.join("theme.toml"), "x").unwrap();
        let now = SystemTime::now();
        assert_eq!(
            prune_cache(&dir, now, PREVIEW_MAX_IDLE, u64::MAX, &[]).unwrap(),
            0
        );
        assert!(st.exists(), "新鲜的解包目录可能正在用，不删");
        let later = now + STAGING_STALE + Duration::from_secs(60);
        assert_eq!(
            prune_cache(&dir, later, PREVIEW_MAX_IDLE, u64::MAX, &[]).unwrap(),
            1
        );
        assert!(!dir.join("staging/123-0-0").exists());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn remove_theme_previews_matches_prefix_exactly() {
        let dir = tmp("forget");
        let zh = file_prefix("主题");
        for n in [
            format!("a.{K}.png"),
            format!("a.b.{K}.png"),
            format!("ab.{K}.png"),
            format!("{zh}.{K}.png"),
            "a.short.png".to_string(),
        ] {
            std::fs::write(dir.join(n), b"x").unwrap();
        }
        assert_eq!(remove_theme_previews(&dir, "a"), 1);
        assert!(dir.join(format!("a.b.{K}.png")).exists());
        assert!(dir.join(format!("ab.{K}.png")).exists());
        assert!(dir.join("a.short.png").exists());
        assert_eq!(remove_theme_previews(&dir, "主题"), 1);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn file_prefix_is_filename_safe_and_distinct() {
        assert_eq!(file_prefix("my-theme.v2_x"), "my-theme.v2_x");
        let a = file_prefix("主题甲");
        let b = file_prefix("主题乙");
        assert_ne!(a, b);
        for p in [&a, &b] {
            assert!(
                p.chars()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
            );
        }
    }

    #[test]
    fn tmp_name_matching_is_exact() {
        assert!(is_preview_tmp(&format!("a.b.{K}.tmp-12-3")));
        assert!(!is_preview_tmp(&format!("a.{K}.tmp-12")));
        assert!(!is_preview_tmp("a.png.tmp-1-0"));
        assert!(!is_preview_tmp(&format!("a.{K}.tmp-x-1")));
    }

    #[test]
    fn png_header_and_completeness() {
        let png = fake_png(300, 40);
        assert_eq!(png_size(&png), Some((300, 40)));
        assert!(png_complete(&png));
        assert!(!png_complete(&png[..png.len() - 1]));
        assert_eq!(png_size(b"not a png at all, no....."), None);
    }

    /// 结构完整的假 PNG（签名 + IHDR + IEND；只用到宽高与收尾校验）。
    fn fake_png(w: u32, h: u32) -> Vec<u8> {
        let mut b = vec![0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a, 0, 0, 0, 13];
        b.extend_from_slice(b"IHDR");
        b.extend_from_slice(&w.to_be_bytes());
        b.extend_from_slice(&h.to_be_bytes());
        b.extend_from_slice(&[8, 6, 0, 0, 0, 0, 0, 0, 0]);
        b.extend_from_slice(&[0, 0, 0, 0, b'I', b'E', b'N', b'D', 0xae, 0x42, 0x60, 0x82]);
        b
    }

    fn data_dir() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../data")
    }

    /// 真实主题目录 + 假 UI 线程：`reply` 为真时每个预览请求回一张假图（宽 = 序号），否则只收不回；
    /// 收到的请求都留在 `held` 里（看取消标志用）。
    type Held = Arc<std::sync::Mutex<Vec<Box<CandidatePreviewRequest>>>>;

    fn coord_with_fake_ui(reply: bool) -> (Arc<Coordinator>, Arc<AtomicU64>, Held) {
        let (c, rx) = Coordinator::new_headless_with_ui(Config::default(), Some(&data_dir()));
        let seen = Arc::new(AtomicU64::new(0));
        let held = Arc::new(std::sync::Mutex::new(Vec::new()));
        let (s, hd) = (Arc::clone(&seen), Arc::clone(&held));
        std::thread::spawn(move || {
            while let Ok(cmd) = rx.recv() {
                if let UiCommand::RenderCandidatePreview(req) = cmd {
                    let n = s.fetch_add(1, Ordering::SeqCst) as u32 + 1;
                    if reply {
                        let w = if req.vertical { 7 } else { 100 + n };
                        let _ = req.reply.try_send(Ok(CandidatePreviewImage {
                            width: w,
                            height: 30,
                            png: fake_png(w, 30),
                        }));
                    }
                    hd.lock().unwrap().push(req);
                }
            }
        });
        (c, seen, held)
    }

    fn ok(v: &Value) -> bool {
        v["ok"].as_bool() == Some(true)
    }

    #[test]
    fn batch_renders_misses_once_then_hits_cache() {
        let _g = serial();
        let (c, seen, _) = coord_with_fake_ui(true);
        let cache = tmp("batch");
        let mut items = vec![
            json!({ "name": "default", "style": "light" }),
            json!({ "name": "default", "style": "dark" }),
            json!({ "name": "default", "layout": "vertical", "inline": true }),
            json!({ "name": "no-such-theme" }),
            json!({ "name": "default", "scale": 9 }),
            json!({ "name": "default", "text": "x" }),
            json!({ "name": "../default" }),
            json!({ "text": "x".repeat(MAX_TEXT_BYTES + 1) }),
        ];
        // 超过在途上限的未命中：按波次投递，全都画出来，不回「忙」。
        for s in [0.5, 0.75, 1.25, 1.5, 1.75, 2.0, 2.5, 3.0, 3.5, 4.0] {
            items.push(json!({ "name": "jade", "scale": s }));
        }
        let first = render_batch(c.as_ref(), &items, Some(&cache));
        assert_eq!(first.len(), items.len());
        assert!(ok(&first[0]) && ok(&first[1]) && ok(&first[2]), "{first:?}");
        for (i, v) in first.iter().enumerate().take(8).skip(3) {
            assert!(!ok(v), "第 {i} 条应失败：{v}");
        }
        assert!(first[8..].iter().all(ok), "{first:?}");
        assert_eq!(seen.load(Ordering::SeqCst), 13, "未命中各渲染一次");
        assert_eq!(IN_FLIGHT.load(Ordering::SeqCst), 0, "名额全部归还");
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
        assert_eq!(seen.load(Ordering::SeqCst), 13, "全部命中，不再请求 UI");
        for (a, b) in first.iter().zip(&again) {
            assert_eq!(a["hash"], b["hash"]);
            assert_eq!(a["width"], b["width"], "宽高从 PNG 头读回");
            assert_eq!(b["cached"], true);
        }
        // 缓存文件坏了（截断）：删掉重画。
        let bytes = std::fs::read(&path).unwrap();
        std::fs::write(&path, &bytes[..bytes.len() - 4]).unwrap();
        let fixed = render_batch(c.as_ref(), &items[..1], Some(&cache));
        assert_eq!(fixed[0]["cached"], false);
        assert_eq!(seen.load(Ordering::SeqCst), 14);
        assert!(png_complete(&std::fs::read(&path).unwrap()));
        assert_eq!(remove_theme_previews(&cache, "default"), 3);
        let _ = std::fs::remove_dir_all(cache);
    }

    #[test]
    fn package_and_text_sources_render_and_clean_staging() {
        let _g = serial();
        let (c, seen, _) = coord_with_fake_ui(true);
        let cache = tmp("pkg");
        let themes = data_dir().join("themes");
        let pkg = cache.join("jade.wtheme");
        wind_transfer::theme::export_package(&themes.join("jade"), &pkg, None).unwrap();
        let text = std::fs::read_to_string(themes.join("jade/theme.toml")).unwrap();
        let items = [
            json!({ "package": pkg.to_string_lossy() }),
            json!({ "text": text }),
            json!({ "text": "this is = = not toml" }),
            json!({ "package": cache.join("missing.wtheme").to_string_lossy() }),
            json!({ "text": "base = \"../../etc\"\n" }),
        ];
        let out = render_batch(c.as_ref(), &items, Some(&cache));
        assert!(ok(&out[0]) && ok(&out[1]), "{out:?}");
        assert!(!ok(&out[2]) && !ok(&out[3]) && !ok(&out[4]), "{out:?}");
        let name = |v: &Value| {
            Path::new(v["path"].as_str().unwrap())
                .file_name()
                .unwrap()
                .to_string_lossy()
                .into_owned()
        };
        assert!(name(&out[0]).starts_with("_pkg.") && name(&out[1]).starts_with("_text."));
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
    fn timeout_reports_error_cancels_request_and_cleans_staging() {
        let _g = serial();
        let (c, _, held) = coord_with_fake_ui(false);
        let cache = tmp("timeout");
        let text = std::fs::read_to_string(data_dir().join("themes/jade/theme.toml")).unwrap();
        let fast = Timeouts {
            base: Duration::from_millis(200),
            per_miss: Duration::ZERO,
            max: Duration::from_secs(1),
        };
        let t = Instant::now();
        let out = render_batch_with(c.as_ref(), &[json!({ "text": text })], Some(&cache), fast);
        assert!(t.elapsed() < Duration::from_secs(3));
        assert!(
            out[0]["error"].as_str().unwrap().contains("超时"),
            "{out:?}"
        );
        let held = held.lock().unwrap();
        assert_eq!(held.len(), 1);
        assert!(held[0].cancelled(), "超时后置位取消，UI 线程排到它就丢掉");
        assert_eq!(IN_FLIGHT.load(Ordering::SeqCst), 0);
        let staging: Vec<_> = std::fs::read_dir(cache.join("staging"))
            .map(|r| r.flatten().collect())
            .unwrap_or_default();
        assert!(staging.is_empty(), "超时也删临时目录");
        let _ = std::fs::remove_dir_all(cache);
    }

    #[test]
    fn busy_when_other_requests_hold_all_slots() {
        let _g = serial();
        let held: Vec<Slot> = (0..MAX_IN_FLIGHT)
            .map(|_| Slot::acquire().unwrap())
            .collect();
        let (c, seen, _) = coord_with_fake_ui(true);
        let cache = tmp("busy");
        let out = render_batch(c.as_ref(), &[json!({ "name": "default" })], Some(&cache));
        assert!(out[0]["error"].as_str().unwrap().contains("忙"), "{out:?}");
        assert_eq!(seen.load(Ordering::SeqCst), 0);
        drop(held);
        assert_eq!(IN_FLIGHT.load(Ordering::SeqCst), 0);
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
        let e = c
            .web_data_rpc("theme.delete", &json!({ "name": "../../x" }))
            .unwrap_err();
        assert!(e.to_string().contains("slug"), "{e}");
    }

    #[test]
    fn without_ui_thread_reports_error_instead_of_hanging() {
        let _g = serial();
        let c = Coordinator::new_headless(Config::default(), Some(&data_dir()));
        let cache = tmp("noui");
        let t = Instant::now();
        let out = render_batch(c.as_ref(), &[json!({ "name": "default" })], Some(&cache));
        assert!(!ok(&out[0]));
        assert!(t.elapsed() < Duration::from_secs(2), "通道断开应立即返回");
        let _ = std::fs::remove_dir_all(cache);
    }
}
