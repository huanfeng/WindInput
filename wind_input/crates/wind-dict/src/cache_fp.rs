//! 缓存有效性：基于源文件**内容指纹**而非 mtime。
//!
//! 痛点：词库源 mtime 会被 scp/部署/版本控制刷新，导致 mtime 校验恒失效 → 每次重建
//! (300MB、耗时)。改用内容指纹后，只要源**内容**未变即复用缓存。
//!
//! 用法（`tag` 标明「这份缓存是按什么方式解析出来的」，读写必须一致）：
//!   - 加载前：`cache_is_fresh(cache, sources, tag)` 为 true 则直接用缓存；
//!   - 构建后：`write_cache_fp(cache, sources, tag)` 写指纹 sidecar 供下次校验。
//!
//! 指纹用 std SipHash（DefaultHasher）：仅做变更检测，非加密用途，足够且无额外依赖。

use std::hash::Hasher;
use std::path::{Path, PathBuf};

/// 指纹 sidecar 路径：`<cache>.fp`（紧贴缓存文件，随缓存一起增删）。
fn fp_sidecar(cache: &Path) -> PathBuf {
    let mut s = cache.as_os_str().to_os_string();
    s.push(".fp");
    PathBuf::from(s)
}

/// **解析语义版本**：改动会影响「同样的源文件解析出什么结果」时必须 +1。
///
/// 指纹原本只覆盖源数据，于是缓存回答的是「源文件变了吗」，而真正该回答的是
/// 「这份缓存和当前程序会产出的结果一致吗」。二者在解析器被修复时会分叉：源文件没变
/// → 指纹不变 → 复用旧缓存 → **解析器修复对存量用户静默失效**，且表现为「明明改了却
/// 没生效」这种最难排查的样子。把语义版本混进指纹，修复即自动重建。
///
/// 历史：
/// - 1 = 初始（列序逐行按 ASCII 猜）
/// - 2 = 列序改为文件级判定：读头部 `columns:` 声明，无声明则整文件投票探测列序、
///   权重仍按 librime 默认取第 3 列（纯 ASCII 词条如 `@`、`$CC(...)` 不再被误判成编码列）
/// - 3 = 只剥行尾空白（前导 U+3000 等不再被当缩进削掉）、空 text/code 跳过、
///   音节语义补上「code 列含空格」这条正面证据（编码在前的拼音库不再丢简拼与边界）、
///   `columns:` 支持流式写法且残缺声明改为整库跳过
/// - 4 = 词条文本的反转义对**命令栏语法条目**只还原换行/制表，反斜杠原样穿过
///   （`$CC(..., open("D:\\notes"))` 不再被本层与 cmdbar lexer 各吃一个反斜杠）
const PARSE_SEMANTICS_VERSION: u32 = 4;

/// 流式读取时的喂料缓冲区大小：足够大以摊薄 syscall 次数，又不至于把峰值分配
/// 重新做回「文件大小」量级——这正是 [`fingerprint`] 从 `std::fs::read` 整读
/// 改为流式的目的（真机实测：拼音方案单次校验峰值 11.6 MB → 1.5 MB）。
const HASH_CHUNK_SIZE: usize = 64 * 1024;

/// 计算源文件集合的内容指纹：混入解析语义版本 + 调用方 tag + 每个源的 文件名/存在性/长度/内容。
///
/// `tag` 用于区分「同一份源文件、但解析方式不同」的缓存。**没有它就会出现这种静默错误**：
/// 把某词库的 `dict_type` 在 english ↔ 非 english 之间切换，只改变 `lowercase_code`
/// 而 `.yaml` 字节不变 → 指纹命中 → 永久复用大小写错误的缓存。
/// 同理，不同种类的缓存（词库 / 注释库）也应各自持 tag，免得共用一个语义版本号
/// 却各改各的、谁也没动机去 +1。
///
/// # 源文件缺失 ≠ 指纹失败（2026-08-24 修）
///
/// **「缺一个源」是用户可达的日常状态**——方案声明了 `default_enabled` 的扩展词库、
/// 而用户没装那个文件；此时该词库对构建产物的贡献就是「什么都没有」，是个**确定且稳定**
/// 的事实，理应可以缓存。
///
/// 此前实现是 `std::fs::read(p).ok()?`：任一源读不到 → 整份指纹 `None` →
/// `write_cache_fp` 什么都不写、`cache_is_fresh` 恒 false → 调用方**每次全量重建**。
/// 真机现场（feihuzj2 方案，11 个词库里 `feihuzj2_extra_gr.dict.yaml` 不存在）
/// 因此每次引擎重建都要重算 30 秒的 `combined.wdat`，且磁盘上从来没有过它的 `.fp`
/// ——那正是本故障唯一的外部指纹。
///
/// 现在把**存在性本身**编进哈希：缺失记 `0`、存在记 `1` + 长度 + 内容。于是
/// 「一直缺」指纹稳定可复用，而「后来补上了」指纹随之改变、缓存正确失效。
///
/// ⚠️ 只有 `NotFound` 按「稳定地不存在」处理。**其余 IO 错误（权限、磁盘故障）仍返回
/// `None`**：那是「读不出来」而非「不存在」，把一次瞬时故障固化进指纹，会让故障恢复后
/// 继续复用错误缓存。
fn fingerprint(sources: &[&Path], tag: &str) -> Option<String> {
    use std::io::Read;

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
                // 长度字段必须排在内容之前写入（见上方 framing 说明），但流式读取
                // 只能边读边喂、读完才知道真实字节数——于是长度只能先从 metadata 拿。
                // 这就打开了一道窄窗口：文件在 open 之后、读完之前被并发改写
                // （部署脚本 scp 覆盖之类），metadata 报的长度可能与实际读到的字节数
                // 对不上。宁可在这种情况下强制重建，也不能把错位的长度悄悄哈希进去
                // ——那会制造出「同一份 fingerprint 对应两种不同内容」的静默错误，
                // 比多花一次重建代价更难排查。
                let declared_len = match f.metadata() {
                    Ok(m) => m.len(),
                    Err(_) => return None,
                };
                h.write_u8(1); // 存在
                h.write_u64(declared_len);

                // 缓冲区放堆上：本函数会在任意线程（含默认 2 MB 栈的工作线程）上跑，
                // 64 KB 栈数组虽通常无碍，但这点分配相对于省下的整份文件不值一提。
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

/// 缓存是否可复用：缓存文件存在 且 指纹 sidecar 与当前源内容+tag 一致。
/// `tag` 见 [`fingerprint`]，必须与写入时一致。
pub fn cache_is_fresh(cache: &Path, sources: &[&Path], tag: &str) -> bool {
    if !cache.exists() {
        return false;
    }
    let Some(fp) = fingerprint(sources, tag) else {
        return false;
    };
    matches!(std::fs::read_to_string(fp_sidecar(cache)), Ok(s) if s.trim() == fp)
}

/// 缓存构建成功后调用：写入指纹 sidecar。
///
/// 单次失败只是「下次多重建一次」，但**持续失败就是持续重建**——大词库上那是几十秒的
/// 同步卡顿，而此前这里是 `let _ = ...` 完全静默，故障只能靠「磁盘上没有 .fp」这种
/// 极隐蔽的方式被发现（真机上正是如此）。两条失败路径都留痕。
pub fn write_cache_fp(cache: &Path, sources: &[&Path], tag: &str) {
    let Some(fp) = fingerprint(sources, tag) else {
        tracing::warn!(
            "无法为 {} 计算源指纹（有源文件读取失败），本次不写 .fp —— \
             该缓存下次仍会全量重建。",
            cache.display()
        );
        return;
    };
    if let Err(e) = std::fs::write(fp_sidecar(cache), fp) {
        tracing::warn!(
            "写入指纹 sidecar 失败 {}: {} —— 缓存已建好但下次仍会全量重建（大词库为数十秒）。",
            fp_sidecar(cache).display(),
            e
        );
    }
}

// ───────────────── 二级缓存：以「缓存产物」而非「源文件」为源 ─────────────────

/// 某个缓存产物的**稳定摘要**，供二级缓存（由缓存派生的缓存）校验用。
///
/// # 为什么二级缓存不能直接哈希源文件
///
/// 反查索引 `.wridx` 派生自一个方案的全部 `.wdat`。若照 [`fingerprint`] 的做法去读
/// **yaml 源**，feihuzj2 那种方案每次启动都要读满 250 MB 才能回答「缓存还能不能用」
/// ——而复用命中时本该是零成本。
///
/// 但每个 `.wdat` 自己就有一份 `.fp`，那 16 个十六进制字符已经编码了
/// 「解析语义版本 + tag + 该 yaml 的全部内容」。于是二级缓存只要哈希这些摘要，
/// 判别力与直接读源**完全相同**，I/O 却从几百 MB 降到几百字节。
///
/// # 三级回退，每级都有明确含义
///
/// 1. `fp:<hash>` —— 有指纹 sidecar（正常路径）；
/// 2. `sz:<len>:<mtime>` —— 无 sidecar（wdat-only 分发的用户投放词库就没有）。
///    这里用 mtime 是安全的：mtime 之所以在源文件上不可信，是因为 scp/部署会刷新它，
///    而**缓存产物与用户投放的二进制只会被整体替换**，替换必然改 mtime。
/// 3. `absent` —— 文件读不到。稳定可哈希，语义是「这一份此刻不在」。
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
/// 与 [`fingerprint`] 不同，**它不会失败**——摘要本身已经把「读不到」表达成了
/// `absent`（见 [`cache_digest`]），故没有「无法判定」这一态。
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
/// [`cache_is_fresh`]（**摘要的顺序参与哈希**：词库次序变了索引内容也会变）。
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
/// ⚠️ **序列化产出的字节语义一变就必须 +1**。文件头里的格式 `VERSION` 只挡得住**布局**
/// 变化，挡不住「同样的布局、不同的内容」——而后者同样会让存量缓存变成错的。
///
/// 这不是假想：本格式落地当天就撞上一次。权重聚合从「按 (词,码) 去重**之后**取 max」
/// 改成「去重**之前**取 max」，wubi86 的索引前后都是 2338080 字节、布局一位没动，
/// `VERSION` 对它完全无感。当时还没发布，直接清掉存量文件即可；**一旦发布，
/// 同类改动就只能靠这里 +1**，否则用户机器上那份错索引会被永久复用。
///
/// 历史：
/// - v1 = 初始（含「权重在去重前聚合」的语义；发布前的中间态未单独计版）
pub const REVERSE_INDEX_TAG: &str = "reverse-index/v1";

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
pub const COMMENT_TAG: &str = "comment/v1";

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn tmp(name: &str, content: &[u8]) -> PathBuf {
        let p = std::env::temp_dir().join(format!("wind_fp_test_{name}"));
        let mut f = std::fs::File::create(&p).unwrap();
        f.write_all(content).unwrap();
        // 清掉上轮可能残留的指纹 sidecar，确保固定 temp 路径下测试可重复（否则
        // 上次 write_cache_fp 写的 `<p>.fp` 会让本轮「未写指纹应不新鲜」误判为新鲜）。
        let mut side = p.clone().into_os_string();
        side.push(".fp");
        let _ = std::fs::remove_file(side);
        p
    }

    /// 语义版本参与指纹：源内容不变、但解析语义版本变了，缓存必须失效。
    /// （否则解析器修复对存量用户静默不生效——本项目真实踩过。）
    #[test]
    fn parse_semantics_version_participates_in_fingerprint() {
        let src = tmp("semver_src.txt", b"same content");
        let fp_now = fingerprint(&[&src], "t").unwrap();
        // 复算一份「仅版本号不同」的指纹：与 fingerprint() 严格同构（含 tag 部分），
        // 只把版本 +1——否则差异可能来自别处，测试就名不副实了。
        let mut h = std::collections::hash_map::DefaultHasher::new();
        h.write_u32(PARSE_SEMANTICS_VERSION + 1);
        h.write(b"t");
        h.write_u8(0xfe);
        let data = std::fs::read(&src).unwrap();
        h.write(src.file_name().unwrap().to_string_lossy().as_bytes());
        h.write_u64(data.len() as u64);
        h.write(&data);
        h.write_u8(0xff);
        let fp_other = format!("{:016x}", h.finish());
        assert_ne!(
            fp_now, fp_other,
            "同样的源内容，语义版本不同必须得到不同指纹"
        );
    }

    /// tag 参与指纹：同一份源、不同解析方式（如 english 词库的 code 小写化）
    /// 必须落到不同指纹，否则切换 dict_type 会永久复用大小写错误的缓存。
    #[test]
    fn tag_participates_in_fingerprint() {
        let src = tmp("tag_src.txt", b"same content");
        let raw = fingerprint(&[&src], dict_tag(false)).unwrap();
        let lower = fingerprint(&[&src], dict_tag(true)).unwrap();
        assert_ne!(raw, lower, "lowercase 与否必须得到不同指纹");
        assert_ne!(
            raw,
            fingerprint(&[&src], COMMENT_TAG).unwrap(),
            "不同种类缓存必须得到不同指纹"
        );

        // 端到端：用 raw tag 写的指纹，不该被 lowercase tag 判为新鲜
        let cache = tmp("tag_src.cache", b"<built>");
        write_cache_fp(&cache, &[&src], dict_tag(false));
        assert!(cache_is_fresh(&cache, &[&src], dict_tag(false)));
        assert!(
            !cache_is_fresh(&cache, &[&src], dict_tag(true)),
            "tag 不一致时必须判定为不新鲜"
        );
    }

    /// ⚠️ 回归：**跨读缓冲区边界的大文件，流式哈希必须与整读等价**。
    ///
    /// `fingerprint` 从 `std::fs::read` 整读改为 64 KB 分块流式（峰值 11.6 MB → 1.5 MB），
    /// 而本模块其余用例的源文件都只有几十字节，**只走得到单块路径**——分块拼接一旦写错
    /// （少喂尾块、多喂一次、长度字段错位），小文件全都照常通过，真实词库（base.dict.yaml
    /// 近 10 MB）却会静默换指纹，表现为**全部存量缓存一次性失效、每次启动重建几十秒**。
    ///
    /// 三个尺寸分别压住：恰好一块、块边界 ±1、多块且尾块不满。
    #[test]
    fn streaming_hash_matches_whole_read_across_chunk_boundaries() {
        for (label, size) in [
            ("恰好一块", HASH_CHUNK_SIZE),
            ("一块少一字节", HASH_CHUNK_SIZE - 1),
            ("一块多一字节", HASH_CHUNK_SIZE + 1),
            ("多块且尾块不满", HASH_CHUNK_SIZE * 3 + 12345),
        ] {
            // 内容刻意非均质：全同字节会让「块顺序颠倒」之类的错误照样通过。
            let content: Vec<u8> = (0..size).map(|i| (i % 251) as u8).collect();
            let src = tmp(&format!("chunk_{size}.bin"), &content);

            // 参考值：按改动前的整读方式逐字节复算，与 fingerprint() 严格同构。
            let mut h = std::collections::hash_map::DefaultHasher::new();
            h.write_u32(PARSE_SEMANTICS_VERSION);
            h.write(b"t");
            h.write_u8(0xfe);
            h.write(src.file_name().unwrap().to_string_lossy().as_bytes());
            h.write_u8(1);
            h.write_u64(content.len() as u64);
            h.write(&content);
            h.write_u8(0xff);
            let whole_read = format!("{:016x}", h.finish());

            assert_eq!(
                fingerprint(&[&src], "t").unwrap(),
                whole_read,
                "{label}（{size} 字节）：流式哈希与整读结果必须一致，否则存量缓存全体失效"
            );
            let _ = std::fs::remove_file(&src);
        }
    }

    #[test]
    fn fresh_only_when_content_matches() {
        let src = tmp("src.txt", b"hello dict");
        let cache = tmp("src.cache", b"<built>");
        // 未写指纹 → 不新鲜
        assert!(!cache_is_fresh(&cache, &[&src], "t"));
        // 写指纹后 → 新鲜
        write_cache_fp(&cache, &[&src], "t");
        assert!(cache_is_fresh(&cache, &[&src], "t"));
    }

    /// ⚠️ 回归：**源文件稳定缺失时，缓存必须照常可复用**。
    ///
    /// 真机现场（2026-08-24，feihuzj2 方案）：11 个词库里 `feihuzj2_extra_gr.dict.yaml`
    /// 不存在（用户/安装目录均无），而它 `default_enabled = true` 故仍进 `sources`。
    /// 旧实现 `std::fs::read(p).ok()?` 让整份指纹变 `None` ⇒ `.fp` 永不写、
    /// `cache_is_fresh` 恒 false ⇒ `combined.wdat` **每次引擎重建都要重算 30 秒**。
    /// 磁盘上「其余词库 `.fp` 都在、唯独 combined 没有」是该故障唯一的外部指纹。
    ///
    /// 三条断言对应三种状态迁移，缺一条这个 bug 都可能以另一种形态回来。
    #[test]
    fn absent_source_is_hashable_and_reappearing_invalidates() {
        let present = tmp("absent_src_present.txt", b"hello");
        let absent = std::env::temp_dir().join("wind_fp_test_absent_then_added");
        let _ = std::fs::remove_file(&absent);

        // ① 缺失不再毒化指纹——能算出来，才谈得上缓存。
        let fp_absent =
            fingerprint(&[&present, &absent], "t").expect("源稳定缺失应能算出指纹，而不是整份失败");

        // ② 端到端：一直缺 → 指纹稳定 → 缓存可复用（这是修复的核心收益）。
        let cache = tmp("absent_src.cache", b"<built>");
        write_cache_fp(&cache, &[&present, &absent], "t");
        assert!(
            cache_is_fresh(&cache, &[&present, &absent], "t"),
            "源稳定缺失时缓存必须可复用，否则就是那个 30 秒重建的 bug"
        );

        // ③ 缺失的文件后来被补上 → 指纹必须改变，否则新词库静默不生效。
        //    （这正是「直接把缺失源剔除出指纹输入」那种修法会漏掉的一面。）
        std::fs::write(&absent, b"now i exist").unwrap();
        let fp_present = fingerprint(&[&present, &absent], "t").unwrap();
        assert_ne!(
            fp_absent, fp_present,
            "文件从无到有必须让指纹改变，缓存才会正确失效"
        );
        assert!(!cache_is_fresh(&cache, &[&present, &absent], "t"));

        let _ = std::fs::remove_file(&absent);
    }

    /// 非 NotFound 的 IO 错误仍须强制重建：那是「读不出来」，不是「不存在」。
    /// 用目录冒充文件来制造一个稳定的非 NotFound 错误（读目录必失败，且跨平台可复现）。
    #[test]
    fn unreadable_non_missing_source_still_forces_rebuild() {
        let dir_as_src = std::env::temp_dir().join("wind_fp_test_dir_as_source");
        std::fs::create_dir_all(&dir_as_src).unwrap();
        assert!(
            fingerprint(&[&dir_as_src], "t").is_none(),
            "读取失败（非 NotFound）必须让指纹失败，不能固化进哈希"
        );
        let _ = std::fs::remove_dir_all(&dir_as_src);
    }

    #[test]
    fn mtime_change_keeps_fresh_content_change_invalidates() {
        let src = tmp("src2.txt", b"content A");
        let cache = tmp("src2.cache", b"<built>");
        write_cache_fp(&cache, &[&src], "t");
        // 仅改 mtime（重写相同内容）→ 仍新鲜（这正是修复点）
        std::fs::write(&src, b"content A").unwrap();
        assert!(cache_is_fresh(&cache, &[&src], "t"));
        // 改内容 → 失效
        std::fs::write(&src, b"content B").unwrap();
        assert!(!cache_is_fresh(&cache, &[&src], "t"));
    }

    #[test]
    fn missing_cache_not_fresh() {
        let src = tmp("src3.txt", b"x");
        let cache = std::env::temp_dir().join("wind_fp_test_nope.cache");
        let _ = std::fs::remove_file(&cache);
        assert!(!cache_is_fresh(&cache, &[&src], "t"));
    }

    /// 二级缓存摘要的三级回退各有确定取值，且**互不相等**——三者若有两个碰撞，
    /// 「有指纹」与「只有文件」就会被当成同一状态。
    #[test]
    fn cache_digest_falls_back_in_three_distinguishable_levels() {
        let src = tmp("digest_src.txt", b"payload");
        let cache = tmp("digest.cache", b"<built>");
        // ② 无 sidecar → sz:len:mtime
        let by_meta = cache_digest(&cache);
        assert!(
            by_meta.starts_with("sz:"),
            "无指纹应回退到 (长度, mtime): {by_meta}"
        );
        // ① 有 sidecar → fp:<hash>，且优先于 ②
        write_cache_fp(&cache, &[&src], "t");
        let by_fp = cache_digest(&cache);
        assert!(by_fp.starts_with("fp:"), "有指纹时必须优先取指纹: {by_fp}");
        assert_ne!(by_fp, by_meta);
        // ③ 文件不在 → absent
        let gone = std::env::temp_dir().join("wind_fp_test_digest_absent");
        let _ = std::fs::remove_file(&gone);
        let mut side = gone.clone().into_os_string();
        side.push(".fp");
        let _ = std::fs::remove_file(side);
        assert_eq!(cache_digest(&gone), "absent");
    }

    /// 二级缓存端到端：摘要不变则复用，任一摘要变了/顺序变了/tag 变了都必须失效。
    ///
    /// 顺序那条尤其要紧：反查索引的内容依赖词库次序（同词多码的排列），
    /// 只把摘要当无序集合会让「调整扩展库顺序」静默复用错误索引。
    #[test]
    fn derived_cache_tracks_digests_order_and_tag() {
        let cache = tmp("derived.cache", b"<index>");
        let a = "fp:aaaaaaaaaaaaaaaa".to_string();
        let b = "fp:bbbbbbbbbbbbbbbb".to_string();

        write_derived_cache_fp(&cache, &[a.clone(), b.clone()], REVERSE_INDEX_TAG);
        assert!(derived_cache_is_fresh(
            &cache,
            &[a.clone(), b.clone()],
            REVERSE_INDEX_TAG
        ));
        // 某个词库变了
        let b2 = "fp:cccccccccccccccc".to_string();
        assert!(!derived_cache_is_fresh(
            &cache,
            &[a.clone(), b2],
            REVERSE_INDEX_TAG
        ));
        // 顺序变了
        assert!(!derived_cache_is_fresh(
            &cache,
            &[b.clone(), a.clone()],
            REVERSE_INDEX_TAG
        ));
        // 少了一个词库
        assert!(!derived_cache_is_fresh(
            &cache,
            std::slice::from_ref(&a),
            REVERSE_INDEX_TAG
        ));
        // tag 变了（格式/语义升级）。刻意用一个**不可能成为真实 tag** 的串——
        // 写成「下一个版本号」的话，`REVERSE_INDEX_TAG` 升版那天这条会静默变成
        // 「拿同一个 tag 比同一个 tag」，从此恒真。（本测试已经这么绊过一次。）
        assert!(!derived_cache_is_fresh(
            &cache,
            &[a, b],
            "reverse-index/<某个不同的 tag>"
        ));
    }

    /// 二级缓存的「源缺失」是可哈希的稳定状态（`absent`），而**补上之后必须失效**。
    /// 这是一级指纹那条 30 秒重建 bug 的同族形态，在二级上提前钉死。
    #[test]
    fn derived_cache_handles_absent_source_and_its_return() {
        let cache = tmp("derived_absent.cache", b"<index>");
        let present = "fp:1111111111111111".to_string();
        write_derived_cache_fp(&cache, &[present.clone(), "absent".into()], "t");
        assert!(
            derived_cache_is_fresh(&cache, &[present.clone(), "absent".into()], "t"),
            "源稳定缺失时二级缓存必须可复用"
        );
        assert!(
            !derived_cache_is_fresh(&cache, &[present, "fp:2222222222222222".into()], "t"),
            "缺失的源后来出现，必须让缓存失效"
        );
    }
}
