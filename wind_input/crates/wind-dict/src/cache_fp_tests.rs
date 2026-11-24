use super::*;
use std::io::Write;

fn tmp(name: &str, content: &[u8]) -> PathBuf {
    let p = std::env::temp_dir().join(format!("wind_fp_test_{name}"));
    let mut f = std::fs::File::create(&p).unwrap();
    f.write_all(content).unwrap();
    // 清掉上轮可能残留的指纹 sidecar，确保固定 temp 路径下测试可重复（否则
    // 上次 write_cache_fp 写的 `<p>.fp` 会让本轮「未写指纹应不新鲜」误判为新鲜）。
    for ext in [".fp", ".st"] {
        let mut side = p.clone().into_os_string();
        side.push(ext);
        let _ = std::fs::remove_file(side);
    }
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

/// 跨读缓冲区边界的大文件，流式哈希必须与整读等价。
///
/// 其余用例的源文件只有几十字节，只走单块路径；分块拼接写错（少喂尾块、多喂一次、长度错位）
/// 时真实词库会静默换指纹，全部存量缓存失效、每次启动重建。
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

        // 参考值：整读后逐字节复算，与 fingerprint() 严格同构。
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

/// 源文件稳定缺失时，缓存必须照常可复用。
///
/// 启用的词库文件不存在（`default_enabled = true` 仍进 `sources`）时，若整份指纹变 `None`，
/// `.fp` 永不写、`cache_is_fresh` 恒 false，每次引擎重建都要重算合并索引。
/// 三条断言对应三种状态迁移。
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

/// 二级缓存摘要的三级回退各有确定取值，且互不相等——两个碰撞就会把「有指纹」与
/// 「只有文件」当成同一状态。
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
    // tag 变了。刻意用不可能成为真实 tag 的串：写成「下一个版本号」的话，
    // `REVERSE_INDEX_TAG` 升版后这条会变成拿同一个 tag 比同一个 tag，恒真。
    assert!(!derived_cache_is_fresh(
        &cache,
        &[a, b],
        "reverse-index/<某个不同的 tag>"
    ));
}

/// 二级缓存的「源缺失」是可哈希的稳定状态（`absent`），而补上之后必须失效。
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

// ───────────── S1：状态 sidecar 快路径（docs/design/system-dict-cache-improvements.md） ─────────────

/// 把文件 mtime 设成一个确定值，并保证与当前值不同（秒级粒度的文件系统也能区分）。
fn set_mtime(p: &Path, secs_from_epoch: u64) {
    let t = std::time::UNIX_EPOCH + std::time::Duration::from_secs(secs_from_epoch);
    std::fs::File::options()
        .write(true)
        .open(p)
        .unwrap()
        .set_modified(t)
        .unwrap();
}

fn mtime_of(p: &Path) -> std::time::SystemTime {
    std::fs::metadata(p).unwrap().modified().unwrap()
}

/// 快路径：源没动就不算全量指纹，证据是 `FULL_HASHES` 计数不增加。不用「换内容、回拨 mtime」
/// 来证：Unix 上写入必推进 ctime，那种改动本就该被识破。
#[test]
fn fast_path_skips_full_hash_when_stamps_unchanged() {
    let hashes = || FULL_HASHES.with(|c| c.get());
    let src = tmp("st_fast_src.txt", b"hello world");
    set_mtime(&src, 1_700_000_000);
    let cache = tmp("st_fast.cache", b"cache");
    write_cache_fp(&cache, &[&src], "t");
    assert!(st_sidecar(&cache).is_file(), "写指纹时应同时写 .st");

    let c0 = hashes();
    assert!(cache_is_fresh(&cache, &[&src], "t"));
    assert!(cache_is_fresh(&cache, &[&src], "t"));
    assert_eq!(hashes(), c0, "stamp 没变 ⇒ 两次判新鲜都不该算全量指纹");

    // 去掉 .st 就没有快路径了，这时必须算一次。
    std::fs::remove_file(st_sidecar(&cache)).unwrap();
    assert!(cache_is_fresh(&cache, &[&src], "t"));
    assert_eq!(hashes(), c0 + 1, "没有 .st ⇒ 走全量");
    assert!(st_sidecar(&cache).is_file(), "全量判新鲜后应回写 .st");
}

/// 只刷新 mtime、内容不变（scp / 部署 / git checkout）：仍新鲜，且 .st 刷成新 mtime，下次又能走快路径。
/// mtime 只用来跳过算指纹，不用来判重建：误判的代价只是多算一次哈希。
#[test]
fn mtime_refresh_without_content_change_stays_fresh_and_refreshes_st() {
    let src = tmp("st_touch_src.txt", b"content");
    set_mtime(&src, 1_700_000_000);
    let cache = tmp("st_touch.cache", b"cache");
    write_cache_fp(&cache, &[&src], "t");
    let st_before = std::fs::read_to_string(st_sidecar(&cache)).unwrap();

    set_mtime(&src, 1_700_000_100);
    assert!(cache_is_fresh(&cache, &[&src], "t"));
    let st_after = std::fs::read_to_string(st_sidecar(&cache)).unwrap();
    assert_ne!(st_before, st_after, "mtime 变了，.st 应被重写");
    assert!(st_after.contains(&format!(
        "{}",
        mtime_of(&src)
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    )));
    // 第三次：快路径直接命中，.st 不再变。
    assert!(cache_is_fresh(&cache, &[&src], "t"));
    assert_eq!(
        st_after,
        std::fs::read_to_string(st_sidecar(&cache)).unwrap()
    );
}

/// 内容变了、长度没变、mtime 变了（正常编辑）：不新鲜。
#[test]
fn content_change_with_new_mtime_is_stale() {
    let src = tmp("st_edit_src.txt", b"aaaa");
    set_mtime(&src, 1_700_000_000);
    let cache = tmp("st_edit.cache", b"cache");
    write_cache_fp(&cache, &[&src], "t");
    std::fs::write(&src, b"bbbb").unwrap();
    set_mtime(&src, 1_700_000_100);
    assert!(!cache_is_fresh(&cache, &[&src], "t"));
}

/// .st 首行指纹与 .fp 不符（老版本重写过 .fp、或 .st 是上一代缓存留下的）：
/// 忽略 .st，走全量指纹。
#[test]
fn st_with_mismatched_fp_is_ignored() {
    let src = tmp("st_mismatch_src.txt", b"hello world");
    set_mtime(&src, 1_700_000_000);
    let cache = tmp("st_mismatch.cache", b"cache");
    write_cache_fp(&cache, &[&src], "t");
    // 伪造：.fp 换成别的值（模拟老版本按另一套源重写了 .fp），.st 保持原样
    std::fs::write(fp_sidecar(&cache), "0000000000000000").unwrap();
    // 此时 stamp 全部匹配，但 .st 的指纹与 .fp 对不上 ⇒ 不能信 .st ⇒ 全量算出的
    // 指纹也与伪造的 .fp 不等 ⇒ 不新鲜
    assert!(!cache_is_fresh(&cache, &[&src], "t"));
}

/// 源的集合变了（多一个源）但旧的那个 stamp 没变：.st 的源数对不上 ⇒ 不走快路径。
#[test]
fn st_with_different_source_set_is_ignored() {
    let src = tmp("st_set_a.txt", b"a");
    set_mtime(&src, 1_700_000_000);
    let extra = tmp("st_set_b.txt", b"b");
    let cache = tmp("st_set.cache", b"cache");
    write_cache_fp(&cache, &[&src], "t");
    assert!(!cache_is_fresh(&cache, &[&src, &extra], "t"));
}

/// 缺失的源同样有 stamp（「不存在」是稳定状态）：一直缺则快路径照走；补上就失效。
#[test]
fn absent_source_has_stamp_and_appearing_invalidates() {
    let src = tmp("st_absent_src.txt", b"x");
    set_mtime(&src, 1_700_000_000);
    let gone = std::env::temp_dir().join("wind_fp_test_st_absent_gone");
    let _ = std::fs::remove_file(&gone);
    let cache = tmp("st_absent.cache", b"cache");
    write_cache_fp(&cache, &[&src, &gone], "t");
    let st = std::fs::read_to_string(st_sidecar(&cache)).unwrap();
    assert!(
        st.lines().any(|l| l.ends_with("\t0")),
        "缺失源应记为 0: {st}"
    );
    assert!(cache_is_fresh(&cache, &[&src, &gone], "t"));
    std::fs::write(&gone, b"now here").unwrap();
    assert!(!cache_is_fresh(&cache, &[&src, &gone], "t"));
    let _ = std::fs::remove_file(&gone);
}

/// 快路径必须把 tag 绑进去：同一份源、同样的 stamp，换 tag 不能走快路径。
/// （现有 `tag_participates_in_fingerprint` 已覆盖此场景，本测试专门钉住 .st 这一层。）
#[test]
fn st_fast_path_is_bound_to_tag_and_semantics_version() {
    let src = tmp("st_tag_src.txt", b"x");
    set_mtime(&src, 1_700_000_000);
    let cache = tmp("st_tag.cache", b"cache");
    write_cache_fp(&cache, &[&src], "dict/raw");
    let stamps = source_stamps(&[&src]).unwrap();
    let stored = std::fs::read_to_string(fp_sidecar(&cache)).unwrap();
    assert!(st_matches(
        &cache,
        stored.trim(),
        "dict/raw",
        &[&src],
        &stamps
    ));
    assert!(!st_matches(
        &cache,
        stored.trim(),
        "dict/lowercase",
        &[&src],
        &stamps
    ));
    // 伪造一份「上一版解析器」写的 .st：只有版本号不同
    let forged = std::fs::read_to_string(st_sidecar(&cache))
        .unwrap()
        .replacen(
            &format!("v{PARSE_SEMANTICS_VERSION}\t"),
            &format!("v{}\t", PARSE_SEMANTICS_VERSION + 1),
            1,
        );
    std::fs::write(st_sidecar(&cache), forged).unwrap();
    assert!(!st_matches(
        &cache,
        stored.trim(),
        "dict/raw",
        &[&src],
        &stamps
    ));
}

/// 刚写入的源（mtime 在 racy 窗口内）不记 stamp，`.st` 不写；时间戳足够旧才写。
/// 否则同一 mtime 刻度内的第二次同长度写入会骗过快路径。
#[test]
fn racy_recent_mtime_is_not_recorded() {
    let src = tmp("st_racy_src.txt", b"fresh"); // mtime = 现在
    let cache = tmp("st_racy.cache", b"cache");
    write_cache_fp(&cache, &[&src], "t");
    assert!(fp_sidecar(&cache).is_file());
    assert!(!st_sidecar(&cache).is_file(), "racy 窗口内不该写 .st");
    assert!(cache_is_fresh(&cache, &[&src], "t"), "全量指纹仍判新鲜");
    assert!(!st_sidecar(&cache).is_file(), "判新鲜那次同样不该写 .st");

    set_mtime(&src, 1_700_000_000); // 把时间戳拨旧 ⇒ 不再 racy
    assert!(cache_is_fresh(&cache, &[&src], "t"));
    assert!(st_sidecar(&cache).is_file(), "时间戳足够旧后才写 .st");
}

// ───────────── S2：内容寻址槽位 ─────────────

/// 槽位 + 指纹 → `<stem>-<fp12>.<ext>`；多段扩展名只切最后一段；无扩展名也能工作。
#[test]
fn slot_content_path_keeps_stem_and_last_extension() {
    let fp = "0123456789abcdef";
    assert_eq!(
        slot_content_path(Path::new("/c/ns/x.wdat"), fp),
        PathBuf::from("/c/ns/x-0123456789ab.wdat")
    );
    assert_eq!(
        slot_content_path(Path::new("/c/ns/rime_frost.merged.wdat"), fp),
        PathBuf::from("/c/ns/rime_frost.merged-0123456789ab.wdat")
    );
    assert_eq!(
        slot_content_path(Path::new("/c/ns/noext"), fp),
        PathBuf::from("/c/ns/noext-0123456789ab")
    );
}

/// 解析槽位指纹：首次全量算并写 `.st`（源不 racy 时）；第二次走 `.st` 快路径得到同一个
/// 指纹；源内容变了得到另一个指纹。
#[test]
fn resolve_slot_fp_is_stable_and_tracks_content() {
    let src = tmp("slot_src.txt", b"alpha");
    set_mtime(&src, 1_700_000_000);
    let slot = std::env::temp_dir().join("wind_fp_test_slot.wdat");
    let _ = std::fs::remove_file(st_sidecar(&slot));
    let fp1 = resolve_slot_fp(&slot, &[&src], "t").unwrap();
    assert_eq!(fp1.len(), 16);
    assert!(st_sidecar(&slot).is_file(), "不 racy 的源应写 .st");
    assert_eq!(resolve_slot_fp(&slot, &[&src], "t").unwrap(), fp1);
    assert_ne!(resolve_slot_fp(&slot, &[&src], "other-tag").unwrap(), fp1);

    std::fs::write(&src, b"beta!").unwrap(); // 同长度
    set_mtime(&src, 1_700_000_100);
    assert_ne!(resolve_slot_fp(&slot, &[&src], "t").unwrap(), fp1);
}

/// 同槽位只留最近 `keep` 份：`current` 恒保留；旧式非寻址文件及其 `.fp` 清掉；
/// 别的槽位（前缀相同但 stem 不同）与非变体文件不碰。
#[test]
fn prune_slot_keeps_current_plus_newest_and_removes_legacy() {
    let dir = std::env::temp_dir().join(format!("wind_fp_prune_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let slot = dir.join("x.wdat");
    let mk = |name: &str, secs: u64| {
        let p = dir.join(name);
        std::fs::write(&p, b"v").unwrap();
        set_mtime(&p, secs);
        p
    };
    let cur = mk("x-aaaaaaaaaaaa.wdat", 1_700_000_300);
    mk("x-aaaaaaaaaaaa.wdat.fp", 1_700_000_300);
    let newer = mk("x-bbbbbbbbbbbb.wdat", 1_700_000_200);
    let older = mk("x-cccccccccccc.wdat", 1_700_000_100);
    mk("x-cccccccccccc.wdat.fp", 1_700_000_100);
    let legacy = mk("x.wdat", 1_700_000_400);
    let legacy_fp = mk("x.wdat.fp", 1_700_000_400);
    let other_slot = mk("x_extra-dddddddddddd.wdat", 1_700_000_050);
    let st = mk("x.wdat.st", 1_700_000_400);
    let foreign = mk("x-notahexfp.wdat", 1_700_000_000);

    prune_slot(&slot, &cur, 2);

    assert!(cur.is_file(), "current 恒保留");
    assert!(dir.join("x-aaaaaaaaaaaa.wdat.fp").is_file());
    assert!(newer.is_file(), "keep=2：current 之外再留最新的一份");
    assert!(!older.is_file(), "第三份应被清");
    assert!(
        !dir.join("x-cccccccccccc.wdat.fp").is_file(),
        "连同它的 .fp"
    );
    assert!(
        !legacy.is_file() && !legacy_fp.is_file(),
        "旧式文件与 .fp 清掉"
    );
    assert!(other_slot.is_file(), "别的槽位不碰");
    assert!(st.is_file(), ".st 不碰");
    assert!(foreign.is_file(), "非十六进制指纹形态的文件不碰");
    let _ = std::fs::remove_dir_all(&dir);
}

/// 被截断、被多写了一行的 `.st` 都不能走快路径。
#[test]
fn truncated_or_overlong_st_is_ignored() {
    let src = tmp("st_trunc_src.txt", b"hello world");
    set_mtime(&src, 1_700_000_000);
    let cache = tmp("st_trunc.cache", b"cache");
    write_cache_fp(&cache, &[&src], "t");
    let good = std::fs::read_to_string(st_sidecar(&cache)).unwrap();
    let stamps = source_stamps(&[&src]).unwrap();
    let fp = std::fs::read_to_string(fp_sidecar(&cache)).unwrap();
    assert!(st_matches(&cache, fp.trim(), "t", &[&src], &stamps));

    // 截断：最后一行只剩一半
    std::fs::write(st_sidecar(&cache), &good[..good.len() - 5]).unwrap();
    assert!(!st_matches(&cache, fp.trim(), "t", &[&src], &stamps));
    // 多一行：按另一组源写的
    std::fs::write(st_sidecar(&cache), format!("{good}extra.txt\t0\n")).unwrap();
    assert!(!st_matches(&cache, fp.trim(), "t", &[&src], &stamps));
}

/// racy 的基准是取 stamp 之前的时刻：哈希期间无论过去多久，取 stamp 那一刻起
/// 2 秒内的 mtime 都算 racy。
#[test]
fn racy_basis_is_the_moment_before_stamping() {
    let now = std::time::SystemTime::now();
    let secs = now.duration_since(std::time::UNIX_EPOCH).unwrap().as_secs();
    let src = tmp("st_basis_src.txt", b"x");
    set_mtime(&src, secs - 1); // 距 basis 1 秒：在窗口内
    let stamps = source_stamps(&[&src]).unwrap();
    assert!(stamps_are_racy(&stamps, now), "距基准 1 秒应算 racy");
    // 哈希「花了」10 秒：若错用哈希后的时刻作基准，就会判成不 racy
    let later = now + std::time::Duration::from_secs(10);
    assert!(
        !stamps_are_racy(&stamps, later),
        "前提：换成之后的时刻就判不出来"
    );
    set_mtime(&src, secs - 10);
    let stamps = source_stamps(&[&src]).unwrap();
    assert!(!stamps_are_racy(&stamps, now), "距基准 10 秒不算 racy");
}

/// Unix 上换文件（rename 进来的新 inode）即使 len 与 mtime 都一样也不走快路径。
/// rename 与 `set_mtime` 本身都会推进 ctime，所以这里守的是「换文件即失效」，不是 ino 字段单独的作用。
#[cfg(unix)]
#[test]
fn replacing_the_file_invalidates_fast_path() {
    let src = tmp("st_ino_src.txt", b"hello world");
    set_mtime(&src, 1_700_000_000);
    let cache = tmp("st_ino.cache", b"cache");
    write_cache_fp(&cache, &[&src], "t");
    assert!(cache_is_fresh(&cache, &[&src], "t"));
    // 另建一个文件再 rename 过去：新 inode，内容等长不同，mtime 回拨到一样
    let tmp_new = std::env::temp_dir().join("wind_fp_test_st_ino_new.txt");
    std::fs::write(&tmp_new, b"HELLO WORLD").unwrap();
    std::fs::rename(&tmp_new, &src).unwrap();
    set_mtime(&src, 1_700_000_000);
    assert!(
        !cache_is_fresh(&cache, &[&src], "t"),
        "inode 变了必须读内容"
    );
}

/// 删指纹 sidecar 时 .st 一起删，免得下一代缓存捡到上一代的 stamp。
#[test]
fn remove_cache_fp_removes_st_too() {
    let src = tmp("st_rm_src.txt", b"x");
    set_mtime(&src, 1_700_000_000); // 不 racy，才会写 .st
    let cache = tmp("st_rm.cache", b"cache");
    write_cache_fp(&cache, &[&src], "t");
    assert!(st_sidecar(&cache).is_file());
    remove_cache_fp(&cache);
    assert!(!st_sidecar(&cache).is_file());
    assert!(!fp_sidecar(&cache).is_file());
}
