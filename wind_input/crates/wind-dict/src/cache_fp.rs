//! 缓存有效性：基于源文件内容指纹而非 mtime（mtime 会被 scp / 部署 / 版本控制刷新，
//! 按它判定会导致每次重建）。
//!
//! 用法（`tag` 标明「这份缓存是按什么方式解析出来的」，读写必须一致）：
//!   - 加载前：`cache_is_fresh(cache, sources, tag)` 为 true 则直接用缓存；
//!   - 构建后：`write_cache_fp(cache, sources, tag)` 写指纹 sidecar 供下次校验。
//!
//! 指纹用 std SipHash（DefaultHasher）：仅做变更检测，非加密用途，足够且无额外依赖。
//!
//! # 状态 sidecar `.st`：源没动就不读内容（S1）
//!
//! `.st` 记下这份指纹是在哪组源状态（存在性、长度、mtime 等）下算出来的。判新鲜先 stat，
//! 逐项相同就信 `.fp` 不读内容；否则算完整指纹，相等时把 `.st` 刷成当前状态。这里 mtime
//! 只决定要不要重算指纹，不决定要不要重建。见 `docs/design/system-dict-cache-improvements.md` S1。

use std::hash::Hasher;
use std::path::{Path, PathBuf};

/// 指纹 sidecar 路径：`<cache>.fp`（紧贴缓存文件，随缓存一起增删）。
fn fp_sidecar(cache: &Path) -> PathBuf {
    let mut s = cache.as_os_str().to_os_string();
    s.push(".fp");
    PathBuf::from(s)
}

/// 状态 sidecar 路径：`<cache>.st`。
fn st_sidecar(cache: &Path) -> PathBuf {
    let mut s = cache.as_os_str().to_os_string();
    s.push(".st");
    PathBuf::from(s)
}

/// 一个源文件的瞬时状态。`Absent` 是稳定可比的状态（与 [`fingerprint`] 对缺失源的
/// 处理一致），不是失败。
///
/// `ino` / `ctime_nanos` 只在 Unix 上有意义（其余平台恒 0）：Flatpak / Nix / `SOURCE_DATE_EPOCH`
/// 打包会把 mtime 钉死，等长修订会被 len+mtime 漏过；换文件必换 inode、ctime 用户态改不了。
/// 代价是 chmod 之类也会多算一次哈希。
///
/// 源存在但读不了（权限、独占锁）时快路径仍判新鲜，这是有意的：读不了也建不出新缓存，
/// 沿用旧缓存好过无候选。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum SourceStamp {
    Absent,
    Present {
        len: u64,
        mtime_nanos: u128,
        ino: u64,
        ctime_nanos: i128,
    },
}

/// 取一个源的状态。`None` = IO 故障或平台给不出 mtime —— 这两种情况都没有快路径可言。
fn source_stamp(p: &Path) -> Option<SourceStamp> {
    match std::fs::metadata(p) {
        Ok(m) => {
            let mtime_nanos = m
                .modified()
                .ok()?
                .duration_since(std::time::UNIX_EPOCH)
                .ok()?
                .as_nanos();
            #[cfg(unix)]
            let (ino, ctime_nanos) = {
                use std::os::unix::fs::MetadataExt;
                (
                    m.ino(),
                    m.ctime() as i128 * 1_000_000_000 + m.ctime_nsec() as i128,
                )
            };
            #[cfg(not(unix))]
            let (ino, ctime_nanos) = (0u64, 0i128);
            Some(SourceStamp::Present {
                len: m.len(),
                mtime_nanos,
                ino,
                ctime_nanos,
            })
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Some(SourceStamp::Absent),
        Err(_) => None,
    }
}

/// 全部源的状态；任一取不到即 `None`。
fn source_stamps(sources: &[&Path]) -> Option<Vec<SourceStamp>> {
    sources.iter().map(|p| source_stamp(p)).collect()
}

/// 一组源的状态快照（不透明），供调用方复核解析前后源没被改写：指纹在解析之前算，期间源
/// 被保存的话，按旧指纹命名的文件会装着新内容。不一致就别按这个指纹落盘。
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct SourceStamps(Option<Vec<SourceStamp>>);

pub fn stamps_of(sources: &[&Path]) -> SourceStamps {
    SourceStamps(source_stamps(sources))
}

impl SourceStamps {
    /// 与另一份快照逐项相同（任一取不到即视为不同，宁可不落盘）。
    pub fn unchanged_since(&self, earlier: &SourceStamps) -> bool {
        matches!((&self.0, &earlier.0), (Some(a), Some(b)) if a == b)
    }
}

/// `.st` 里记的源标识用全路径：自带库与用户覆盖库共用槽位，Windows 上没有 inode / ctime，
/// 只靠 len + mtime 区分两份文件太薄。
fn source_name(p: &Path) -> String {
    p.to_string_lossy().into_owned()
}

/// `.st` 的文本形式：首行指纹，次行 `v<解析语义版本>\t<tag>`，其后每源一行
/// `name\t1\tlen\tmtime_nanos\tino\tctime_nanos` 或 `name\t0`。
fn render_st(fp: &str, tag: &str, sources: &[&Path], stamps: &[SourceStamp]) -> String {
    let mut out = String::with_capacity(48 + sources.len() * 48);
    out.push_str(fp);
    out.push('\n');
    // 第二行：解析语义版本 + tag。快路径不重算指纹，不记的话升级解析器或换 tag 后会复用旧缓存。
    out.push_str(&format!("v{PARSE_SEMANTICS_VERSION}\t{tag}\n"));
    for (p, st) in sources.iter().zip(stamps) {
        out.push_str(&source_name(p));
        match st {
            SourceStamp::Absent => out.push_str("\t0"),
            SourceStamp::Present {
                len,
                mtime_nanos,
                ino,
                ctime_nanos,
            } => {
                out.push_str(&format!("\t1\t{len}\t{mtime_nanos}\t{ino}\t{ctime_nanos}"));
            }
        }
        out.push('\n');
    }
    out
}

/// `.st` 记录的指纹等于 `expected_fp` 且其余各项匹配（见 [`st_current_fp`]）时返回 true。
fn st_matches(
    cache: &Path,
    expected_fp: &str,
    tag: &str,
    sources: &[&Path],
    now: &[SourceStamp],
) -> bool {
    st_current_fp(cache, tag, sources, now).as_deref() == Some(expected_fp)
}

/// 读 `.st`：解析语义版本与 tag 相同、源数与名字逐项相同、stamp 逐项相同时，返回它记录
/// 的指纹；否则 `None`。任何解析失败都按 `None` 处理——`.st` 只是加速，不能因它出错而把
/// 判定变错。
fn st_current_fp(
    cache: &Path,
    tag: &str,
    sources: &[&Path],
    now: &[SourceStamp],
) -> Option<String> {
    let text = std::fs::read_to_string(st_sidecar(cache)).ok()?;
    let mut lines = text.lines();
    let fp = lines.next()?.trim();
    if fp.len() != 16 || !fp.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    if lines.next() != Some(format!("v{PARSE_SEMANTICS_VERSION}\t{tag}").as_str()) {
        return None;
    }
    let mut seen = 0usize;
    for (p, st) in sources.iter().zip(now) {
        let line = lines.next()?;
        let mut cols = line.split('\t');
        if cols.next() != Some(source_name(p).as_str()) {
            return None;
        }
        let recorded = match cols.next() {
            Some("0") => SourceStamp::Absent,
            Some("1") => {
                let (Some(len), Some(mt), Some(ino), Some(ct)) = (
                    cols.next().and_then(|v| v.parse::<u64>().ok()),
                    cols.next().and_then(|v| v.parse::<u128>().ok()),
                    cols.next().and_then(|v| v.parse::<u64>().ok()),
                    cols.next().and_then(|v| v.parse::<i128>().ok()),
                ) else {
                    return None;
                };
                SourceStamp::Present {
                    len,
                    mtime_nanos: mt,
                    ino,
                    ctime_nanos: ct,
                }
            }
            _ => return None,
        };
        if recorded != *st {
            return None;
        }
        seen += 1;
    }
    // 源数必须完全一致：`.st` 多出来的行说明它是按另一组源写的。
    (seen == sources.len() && lines.next().is_none()).then(|| fp.to_string())
}

/// 「刚刚被改过」的源不记 stamp：mtime 离现在不足这么久的，下次仍走全量指纹。
///
/// mtime 刻度是粗的（毫秒到秒级），同一刻度内两次同长度写入的 mtime 相同，在第一次写入的
/// 刻度里记下 stamp 会让第二次写入骗过快路径（同 git 的 racy-index 规则）。代价只是刚部署完
/// 的那一次启动多算一遍哈希。
const RACY_WINDOW: std::time::Duration = std::time::Duration::from_secs(2);

/// `stamps` 相对时刻 `basis` 是否 racy。`basis` 必须是取 stamp 之前的时刻：哈希可能超过
/// 窗口，用哈希完成后的「现在」会把取 stamp 之后的改写判成早已过窗。
fn stamps_are_racy(stamps: &[SourceStamp], basis: std::time::SystemTime) -> bool {
    let Ok(basis) = basis.duration_since(std::time::UNIX_EPOCH) else {
        return true;
    };
    let threshold = basis.saturating_sub(RACY_WINDOW).as_nanos();
    stamps.iter().any(|s| match s {
        SourceStamp::Present { mtime_nanos, .. } => *mtime_nanos >= threshold,
        SourceStamp::Absent => false,
    })
}

/// 取 before stamp 之前要先记下的时刻（见 [`stamps_are_racy`]）。
fn stamp_basis() -> std::time::SystemTime {
    std::time::SystemTime::now()
}

/// 写 `.st`。只在哈希前后两次 stat 一致、且没有源处于 racy 窗口内时才写；写失败只记 WARN
/// （没有 `.st` 只是退回全量指纹）。
///
/// racy 判两次：先按系统时钟，写完再按 `.st` 自身的 mtime（与源同一文件系统时钟，防网络盘
/// 服务器时钟比本机慢）。
fn write_st(
    cache: &Path,
    fp: &str,
    tag: &str,
    sources: &[&Path],
    before: &[SourceStamp],
    basis: std::time::SystemTime,
) {
    let st = st_sidecar(cache);
    let Some(after) = source_stamps(sources) else {
        let _ = std::fs::remove_file(&st);
        return;
    };
    if after != before || stamps_are_racy(before, basis) {
        let _ = std::fs::remove_file(&st);
        return;
    }
    if let Err(e) = std::fs::write(&st, render_st(fp, tag, sources, before)) {
        tracing::warn!(
            "写入状态 sidecar 失败 {}: {} —— 下次判新鲜退回全量读源",
            st.display(),
            e
        );
        return;
    }
    // 文件系统时钟基准：`.st` 刚落盘的 mtime 与源的 mtime 同源可比。基准仍要对应
    // 「取 stamp 之前」那一刻——把系统时钟下从 basis 到现在流逝的时长从 st_mtime 里减掉。
    let elapsed = std::time::SystemTime::now()
        .duration_since(basis)
        .unwrap_or_default();
    match std::fs::metadata(&st).and_then(|m| m.modified()) {
        Ok(st_mtime) if !stamps_are_racy(before, st_mtime - elapsed) => {}
        _ => {
            let _ = std::fs::remove_file(&st);
        }
    }
}

/// 解析语义版本：改动会影响「同样的源文件解析出什么结果」时必须 +1，否则源没变、指纹
/// 不变，解析器修复对存量用户静默失效。
///
/// 各版语义：
/// - 1 = 初始（列序逐行按 ASCII 猜）
/// - 2 = 列序文件级判定（`columns:` 声明或整文件投票），权重默认第 3 列
/// - 3 = 只剥行尾空白、空 text/code 跳过、code 列含空格算音节证据、`columns:` 流式写法与残缺声明整库跳过
/// - 4 = 命令栏语法条目反转义只还原换行/制表，反斜杠原样穿过
/// - 5 = text 列保留尾随空白，code/weight 各自 `trim_end`
/// - 6 = 命令栏语法条目落在 code 列时逐行对调列序
const PARSE_SEMANTICS_VERSION: u32 = 6;

#[cfg(test)]
thread_local! {
    /// 测试用：本线程全量指纹计算的次数（线程局部，测试并行时互不干扰）。
    /// 快路径的证据就是「它没有增加」。
    static FULL_HASHES: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// 流式读取的缓冲区大小：摊薄 syscall 次数，又不让峰值分配回到「文件大小」量级。
const HASH_CHUNK_SIZE: usize = 64 * 1024;

/// 计算源文件集合的内容指纹：混入解析语义版本 + 调用方 tag + 每个源的 文件名/存在性/长度/内容。
///
/// `tag` 区分「同一份源文件、但解析方式不同」的缓存（如 `dict_type` 切换 english 只改变
/// 是否小写化，`.yaml` 字节不变）。不同种类的缓存也各持 tag，各自独立 +1。
///
/// # 源文件缺失 ≠ 指纹失败
///
/// 缺源是日常状态（方案声明的扩展词库用户没装），其贡献稳定为空，应可缓存：存在性编进
/// 哈希，缺失记 `0`、存在记 `1` + 长度 + 内容，补上后指纹随之改变。只有 `NotFound` 这样
/// 处理；其余 IO 错误返回 `None`，免得把瞬时故障固化进指纹。
fn fingerprint(sources: &[&Path], tag: &str) -> Option<String> {
    use std::io::Read;
    #[cfg(test)]
    FULL_HASHES.with(|c| c.set(c.get() + 1));

    let mut h = std::collections::hash_map::DefaultHasher::new();
    h.write_u32(PARSE_SEMANTICS_VERSION);
    h.write(tag.as_bytes());
    h.write_u8(0xfe); // tag 与源内容之间的分隔
    for p in sources {
        if let Some(name) = p.file_name() {
            h.write(name.to_string_lossy().as_bytes());
        }
        match std::fs::File::open(p) {
            Ok(mut f) => {
                // 长度须先于内容写入，流式读取只能先从 metadata 拿。读完后与实际字节数比对，
                // 不一致（并发改写）就放弃，免得同一指纹对应两种内容。
                let declared_len = match f.metadata() {
                    Ok(m) => m.len(),
                    Err(_) => return None,
                };
                h.write_u8(1); // 存在
                h.write_u64(declared_len);

                // 缓冲区放堆上：本函数会在任意线程（含小栈工作线程）上跑。
                let mut buf = vec![0u8; HASH_CHUNK_SIZE];
                let mut actual_len: u64 = 0;
                loop {
                    match f.read(&mut buf) {
                        Ok(0) => break,
                        Ok(n) => {
                            h.write(&buf[..n]);
                            actual_len += n as u64;
                        }
                        Err(_) => return None, // 读取中途故障：无法判定，强制重建
                    }
                }
                if actual_len != declared_len {
                    return None; // 读到的字节数与声明长度不一致：并发改写，强制重建
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                h.write_u8(0); // 稳定地不存在——参与哈希，不再毒化整份指纹
            }
            Err(_) => return None, // 权限/IO 故障：无法判定，强制重建
        }
        h.write_u8(0xff); // 分隔，避免相邻源内容拼接歧义
    }
    Some(format!("{:016x}", h.finish()))
}

/// 供 [`crate::build_guard`] 用的构建输入键：与 `.fp` 同一套指纹。源读不出时返回固定的
/// `unreadable`——这种情况下指纹本就无法判定，按「同一份输入」计数即可。
pub fn build_key(sources: &[&Path], tag: &str) -> String {
    fingerprint(sources, tag).unwrap_or_else(|| "unreadable".to_string())
}

/// 二级缓存（如 `.wridx`）的构建输入键，语义同 [`build_key`]。
pub fn derived_build_key(source_digests: &[String], tag: &str) -> String {
    derived_fingerprint(source_digests, tag)
}

/// 缓存是否可复用：缓存文件存在 且 指纹 sidecar 与当前源内容+tag 一致。
/// `tag` 见 [`fingerprint`]，必须与写入时一致。
pub fn cache_is_fresh(cache: &Path, sources: &[&Path], tag: &str) -> bool {
    if !cache.exists() {
        return false;
    }
    let Ok(stored) = std::fs::read_to_string(fp_sidecar(cache)) else {
        return false;
    };
    let stored = stored.trim();

    // 快路径：源的 (存在性, 长度, mtime, ino, ctime) 与 `.st` 记录逐项相同 ⇒ 信 `.fp`，不读内容。
    let basis = stamp_basis();
    let before = source_stamps(sources);
    if let Some(now) = before.as_deref()
        && st_matches(cache, stored, tag, sources, now)
    {
        return true;
    }

    let Some(fp) = fingerprint(sources, tag) else {
        return false;
    };
    if fp != stored {
        return false;
    }
    // 新鲜但 `.st` 没对上（典型：部署只刷新了 mtime）：刷成当前状态，下次走快路径。
    if let Some(before) = before {
        write_st(cache, &fp, tag, sources, &before, basis);
    }
    true
}

/// 删掉缓存的指纹 sidecar（不存在不算错）。重建缓存之前调用：新 `.fp` 写失败时盘上留下
/// 「没有指纹」而不是「旧指纹」，否则派生缓存（`.wridx`、英文词组索引）会把新内容当成没变过。
pub fn remove_cache_fp(cache: &Path) {
    for side in [fp_sidecar(cache), st_sidecar(cache)] {
        if let Err(e) = std::fs::remove_file(&side)
            && e.kind() != std::io::ErrorKind::NotFound
        {
            tracing::warn!("删除旧指纹 sidecar {} 失败: {e}", side.display());
        }
    }
}

/// 缓存构建成功后调用：写入指纹 sidecar。
///
/// 持续失败就是持续重建（大词库几十秒），故两条失败路径都记 WARN。
pub fn write_cache_fp(cache: &Path, sources: &[&Path], tag: &str) {
    let basis = stamp_basis();
    let before = source_stamps(sources);
    let Some(fp) = fingerprint(sources, tag) else {
        // 旧 `.fp` / `.st` 不能留：盘上已经是新缓存，留着旧 sidecar 等于给它配了一份
        // 与内容对不上的指纹，源被带 mtime 还原成旧版时会误复用。
        remove_cache_fp(cache);
        tracing::warn!(
            "无法为 {} 计算源指纹（有源文件读取失败），本次不写 .fp —— \
             该缓存下次仍会全量重建。",
            cache.display()
        );
        return;
    };
    if let Err(e) = std::fs::write(fp_sidecar(cache), &fp) {
        tracing::warn!(
            "写入指纹 sidecar 失败 {}: {} —— 缓存已建好但下次仍会全量重建（大词库为数十秒）。",
            fp_sidecar(cache).display(),
            e
        );
        let _ = std::fs::remove_file(st_sidecar(cache));
        return;
    }
    match before {
        Some(before) => write_st(cache, &fp, tag, sources, &before, basis),
        None => {
            let _ = std::fs::remove_file(st_sidecar(cache));
        }
    }
}

// ───────────────── 内容寻址缓存（S2）：文件名带指纹，文件一旦写成不再改写 ─────────────────
//
// 「槽位」是逻辑缓存路径（如 `<cache>/<ns>/x.wdat`），实际文件是 `<ns>/x-<指纹前 12 位>.wdat`。
// 同名不同内容的源各得各的文件，重建永远写新文件、不覆盖正被 mmap 的旧文件。`.st` 挂在
// 槽位上（`x.wdat.st`），直接给出当前该指向的指纹；`.fp` 仍写在实际文件旁供 [`cache_digest`]。
// 见 `docs/design/system-dict-cache-improvements.md` S2。

/// 文件名里带的指纹长度（十六进制字符数）。48 位对「同一槽位下的几份变体」绰绰有余；
/// 完整 64 位在 `.fp` 里。
const NAME_FP_LEN: usize = 12;

/// 当前这组源（按 `tag` 解析）应当指向的指纹：优先 `.st` 快路径，否则全量算并回写 `.st`。
/// `None` = 源读不出（权限 / IO 故障），此时没有稳定的缓存可言，调用方退回内存模式。
pub fn resolve_slot_fp(slot: &Path, sources: &[&Path], tag: &str) -> Option<String> {
    let basis = stamp_basis();
    let before = source_stamps(sources);
    if let Some(now) = before.as_deref()
        && let Some(fp) = st_current_fp(slot, tag, sources, now)
    {
        return Some(fp);
    }
    let fp = fingerprint(sources, tag)?;
    if let Some(before) = before {
        write_st(slot, &fp, tag, sources, &before, basis);
    }
    Some(fp)
}

/// 槽位 + 指纹 → 实际文件路径：`<dir>/<stem>-<fp12>.<ext>`。`stem` 取槽位文件名最后一个
/// `.` 之前的部分（`x.merged.wdat` → `x.merged`），没有扩展名就整个当 stem。
pub fn slot_content_path(slot: &Path, fp: &str) -> PathBuf {
    let name = slot
        .file_name()
        .map(|n| n.to_string_lossy())
        .unwrap_or_default();
    let short = &fp[..fp.len().min(NAME_FP_LEN)];
    let file = match name.rsplit_once('.') {
        Some((stem, ext)) => format!("{stem}-{short}.{ext}"),
        None => format!("{name}-{short}"),
    };
    slot.with_file_name(file)
}

/// 把已知指纹写成实际文件旁的 `.fp`（内容寻址下指纹已经算过，不再重算）。
/// 失败只记 WARN：没有它，二级缓存的摘要退到「大小:mtime」，仍然正确、只是换一次就重建。
pub fn write_cache_fp_value(cache: &Path, fp: &str) {
    if let Err(e) = std::fs::write(fp_sidecar(cache), fp) {
        tracing::warn!(
            "写入指纹 sidecar 失败 {}: {} —— 派生缓存将按大小:mtime 判定该文件",
            fp_sidecar(cache).display(),
            e
        );
    }
}

/// 复用命中时调用：把 `.fp` 的 mtime 推到现在，作为这份变体的「最近使用」时间。
/// 不碰 wdat 本身——`SystemDictLayer::entries_digest` 把 wdat 的 `大小:mtime` 混进了派生
/// 缓存的摘要，touch 它会让英文词组索引每次命中都白重建一次。
pub fn touch_cache(cache: &Path) {
    if let Ok(f) = std::fs::File::options().write(true).open(fp_sidecar(cache)) {
        let _ = f.set_modified(std::time::SystemTime::now());
    }
}

/// 同一槽位下只留最近 `keep` 份内容寻址文件（`current` 恒保留；最近使用 = max(wdat mtime,
/// `.fp` mtime)），其余连同 `.fp` 删掉，旧式非寻址文件一并清。写盘临时文件不清：分不出是不是
/// 别的进程正在写。best-effort，删不掉（仍被映射）就留到下次。
///
/// `keep = 2`：自带与用户覆盖两份交替使用时都留着，才不会来回重建。
pub fn prune_slot(slot: &Path, current: &Path, keep: usize) {
    let Some(dir) = slot.parent() else {
        return;
    };
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    let slot_name = slot.file_name().map(|n| n.to_string_lossy().into_owned());
    let Some(slot_name) = slot_name else {
        return;
    };
    let (stem, ext) = match slot_name.rsplit_once('.') {
        Some((s, e)) => (s.to_string(), format!(".{e}")),
        None => (slot_name.clone(), String::new()),
    };
    let prefix = format!("{stem}-");
    let is_variant = |name: &str| -> bool {
        let Some(rest) = name.strip_prefix(&prefix) else {
            return false;
        };
        let Some(hex) = rest.strip_suffix(&ext) else {
            return false;
        };
        hex.len() == NAME_FP_LEN && hex.bytes().all(|b| b.is_ascii_hexdigit())
    };
    let mut variants: Vec<(std::time::SystemTime, PathBuf)> = Vec::new();
    for e in rd.flatten() {
        let p = e.path();
        let Some(name) = p.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        if p == current || name.ends_with(".st") {
            continue;
        }
        // 旧式非寻址文件与它的 `.fp`：升级后再无人引用，直接清
        if name == slot_name || name == format!("{slot_name}.fp") {
            let _ = std::fs::remove_file(&p);
            continue;
        }
        if is_variant(name) {
            let mtime = e
                .metadata()
                .and_then(|m| m.modified())
                .unwrap_or(std::time::UNIX_EPOCH);
            let used = std::fs::metadata(fp_sidecar(&p))
                .and_then(|m| m.modified())
                .unwrap_or(std::time::UNIX_EPOCH);
            variants.push((mtime.max(used), p));
        }
    }
    variants.sort_by_key(|v| std::cmp::Reverse(v.0)); // 新的在前；`current` 已占一个名额
    for (_, p) in variants.into_iter().skip(keep.saturating_sub(1)) {
        let _ = std::fs::remove_file(&p);
        let _ = std::fs::remove_file(fp_sidecar(&p));
    }
}

// ───────────────── 二级缓存：以「缓存产物」而非「源文件」为源 ─────────────────

/// 某个缓存产物的稳定摘要，供二级缓存（由缓存派生的缓存，如 `.wridx`）校验用。
///
/// 二级缓存哈希各 `.wdat` 的 `.fp` 而不读 yaml 源：`.fp` 已编码了解析语义版本 + tag + 源内容，
/// 判别力相同，I/O 从几百 MB 降到几百字节。
///
/// 三级回退：
/// 1. `fp:<hash>`：有指纹 sidecar（正常路径）；
/// 2. `sz:<len>:<mtime>`：无 sidecar（wdat-only 投放词库）。缓存产物与投放的二进制只会被
///    整体替换，替换必改 mtime，故这里用 mtime 是安全的；
/// 3. `absent`：文件读不到，语义是「这一份此刻不在」。
pub fn cache_digest(cache: &Path) -> String {
    if let Ok(s) = std::fs::read_to_string(fp_sidecar(cache)) {
        return format!("fp:{}", s.trim());
    }
    let Ok(meta) = std::fs::metadata(cache) else {
        return "absent".to_string();
    };
    let mtime = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    format!("sz:{}:{}", meta.len(), mtime)
}

/// 二级缓存的指纹：混入解析语义版本 + tag + 各源摘要。
///
/// 不会失败：「读不到」已由 [`cache_digest`] 表达成 `absent`。
fn derived_fingerprint(source_digests: &[String], tag: &str) -> String {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    h.write_u32(PARSE_SEMANTICS_VERSION);
    h.write(tag.as_bytes());
    h.write_u8(0xfe);
    for d in source_digests {
        h.write(d.as_bytes());
        h.write_u8(0xff); // 分隔，避免相邻摘要拼接歧义
    }
    format!("{:016x}", h.finish())
}

/// 二级缓存是否可复用。源是 [`cache_digest`] 摘要而非文件路径，其余语义同
/// [`cache_is_fresh`]（摘要的顺序参与哈希：词库次序变了索引内容也会变）。
pub fn derived_cache_is_fresh(cache: &Path, source_digests: &[String], tag: &str) -> bool {
    if !cache.exists() {
        return false;
    }
    let fp = derived_fingerprint(source_digests, tag);
    matches!(std::fs::read_to_string(fp_sidecar(cache)), Ok(s) if s.trim() == fp)
}

/// 二级缓存构建成功后写指纹 sidecar。失败留痕，理由同 [`write_cache_fp`]。
pub fn write_derived_cache_fp(cache: &Path, source_digests: &[String], tag: &str) {
    let fp = derived_fingerprint(source_digests, tag);
    if let Err(e) = std::fs::write(fp_sidecar(cache), fp) {
        tracing::warn!(
            "写入二级缓存指纹 sidecar 失败 {}: {} —— 缓存已建好但下次仍会全量重建。",
            fp_sidecar(cache).display(),
            e
        );
    }
}

/// 反查索引（`.wridx`）的指纹 tag。
///
/// 序列化产出的字节语义一变就必须 +1：文件头的格式 `VERSION` 只挡得住布局变化，挡不住
/// 「同样的布局、不同的内容」（如权重聚合挪到去重前后，字节数一位不差）。
///
/// 各版语义：
/// - v1 = 初始（权重在去重前聚合）
pub const REVERSE_INDEX_TAG: &str = "reverse-index/v1";

/// 单字全码表（`.wscc`）缓存的 tag。同 [`REVERSE_INDEX_TAG`]，取码判据（见
/// [`crate::cached::build_single_char_full_codes_from`]）一改就要 +1。
///
/// 这张表还依赖方案的 `max_code_length`，它不在词库指纹里，调用方须并进 `source_digests`
/// （见 `EngineManager::build_single_char_codes_for`）。
///
/// 各版语义：
/// - v1 = 初始
pub const SINGLE_CHAR_CODES_TAG: &str = "single-char-codes/v1";

/// 英文词组分词索引（`.wphr`，`wind_engine::english_phrase`）的指纹 tag。
///
/// 同 [`REVERSE_INDEX_TAG`]：收词判据、小写化规则、首词排序键任一变了就要 +1。指纹进文件名，
/// +1 即换新文件，旧的由保留上限清走。
///
/// 各版语义：
/// - v1 = 初始（`text` 按空白切词、≥2 词才收、逐词 `lower()`、按首词字节序稳定排序）
pub const ENGLISH_PHRASE_INDEX_TAG: &str = "english-phrase/v1";

/// 词库缓存的 tag：区分 code 列是否被小写化（`dict_type = english` 走小写）。
pub fn dict_tag(lowercase_code: bool) -> &'static str {
    if lowercase_code {
        "dict/lowercase"
    } else {
        "dict/raw"
    }
}

/// 注释库缓存的 tag。同理独立于词库解析：注释库用的是 `wind-reverse` 里那份精简解析器
/// （只取 text/comment/code 三列），与 `codetable` 的 rime 解析各自演进。
///
/// 各版语义：
/// - v1 = 初始
/// - v2 = 无 `...` 的裸表按 OpenCC 约定读（丢重复键的值首项、跳过 `---` 行）
pub const COMMENT_TAG: &str = "comment/v2";

/// emoji 扩展表缓存的 tag（`.wemj`，见 [`crate::emojidict`]）。
///
/// 与 [`COMMENT_TAG`] 分开：本表产出还取决于繁→简归一表（`TSCharactersDerived.octrie`），
/// 调用方须把它一并传进 `sources`，否则 OpenCC 数据升级后旧缓存会被永久复用。
///
/// 各版语义：
/// - v1 = 初始（丢值首项 + 逐字繁→简归一 + 同键合并保序去重）
pub const EMOJI_TAG: &str = "emoji/v1";

#[cfg(test)]
#[path = "cache_fp_tests.rs"]
mod tests;
