//! gen_opencc：把 OpenCC .txt 源词典编译为 .octrie 二进制
//!
//! 用法：gen_opencc --src <dict_dir> --out <out_dir>
//!
//! .octrie 格式与 wind-transform/s2t.rs 一致：
//!   Header(16B): Magic"WIOC" + Version u32 + Count u32 + MaxKeyB u16 + Reserved u16
//!   Entries(N×12B,按key升序): KeyOff u32 + KeyLen u16 + ValOff u32 + ValLen u16
//!   StringPool: UTF-8 字节池（key 段 + val 段，各自连续）

use std::io::Write;
use std::path::{Path, PathBuf};

fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().collect();
    let (src, out) = parse_args(&args)?;
    std::fs::create_dir_all(&out)?;

    let mut total = 0usize;
    let mut entries: Vec<_> = std::fs::read_dir(&src)?
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .collect();
    entries.sort();

    for path in entries {
        if path.extension().and_then(|e| e.to_str()) != Some("txt") {
            continue;
        }
        let stem = match path.file_stem().and_then(|s| s.to_str()) {
            Some(s) => s.to_string(),
            None => continue,
        };
        let dst = out.join(format!("{}.octrie", stem));
        let n = compile_octrie(&path, &dst)?;
        eprintln!("  {} ({} 条) → {}", stem, n, dst.display());
        total += 1;
        // STCharacters 的多值行（1对多变体，如「出→出 齣」）另编一张变体表：
        // key→完整多值串（保留定义序，首个=默认转换结果）。主表仍取首值（OpenCC 转换
        // 语义），变体表仅供候选层 1对多展开查询（wind-transform s2t::Converter::variants_of）。
        if stem == "STCharacters" {
            let vdst = out.join("STVariants.octrie");
            let vn = compile_variants_octrie(&path, &vdst)?;
            eprintln!("  STVariants ({} 条) → {}", vn, vdst.display());
            total += 1;
            // 再反转出一张 繁→简 字表，供「上游词表是繁体、本仓候选恒简体」的场合归一
            // （现有消费者：emoji 扩展表的键归一，见 docs/design/emoji-suggestion.md §3.1）。
            let rdst = out.join("TSCharactersDerived.octrie");
            let rn = compile_t2s_octrie(&path, &rdst)?;
            eprintln!("  TSCharactersDerived ({} 条) → {}", rn, rdst.display());
            total += 1;
        }
    }
    eprintln!("gen_opencc: 编译 {} 个词典完成", total);
    Ok(())
}

fn compile_octrie(src: &Path, dst: &Path) -> anyhow::Result<usize> {
    let content = std::fs::read_to_string(src)?;
    let mut pairs: Vec<(Vec<u8>, Vec<u8>)> = Vec::new();

    for line in content.lines() {
        let Some((key, rest)) = parse_line(line) else {
            continue;
        };
        // 多值空格分隔，取第一个
        let val = rest.split(' ').next().unwrap_or(rest);
        if !val.is_empty() {
            pairs.push((key.as_bytes().to_vec(), val.as_bytes().to_vec()));
        }
    }
    write_octrie(pairs, dst)
}

/// 仅收集**多值行**（值含空格分隔的多个变体），val 保留完整多值串。
fn compile_variants_octrie(src: &Path, dst: &Path) -> anyhow::Result<usize> {
    let content = std::fs::read_to_string(src)?;
    let mut pairs: Vec<(Vec<u8>, Vec<u8>)> = Vec::new();

    for line in content.lines() {
        let Some((key, rest)) = parse_line(line) else {
            continue;
        };
        let vals: Vec<&str> = rest.split(' ').filter(|v| !v.is_empty()).collect();
        if vals.len() > 1 {
            pairs.push((key.as_bytes().to_vec(), vals.join(" ").into_bytes()));
        }
    }
    write_octrie(pairs, dst)
}

/// 反转 `STCharacters.txt` 得 **繁→简** 字表：源行 `简<TAB>繁1 繁2 …` 拆成多条 `繁i → 简`。
///
/// # 为什么反转而不是下载 OpenCC 的 TSCharacters
///
/// 少一个外部依赖，且两者对本用途等价——我们只需要「把繁体键归一到本仓的简体域」，
/// 不需要 OpenCC 那套完整的繁→简语义（异体字取舍、地区变体）。
///
/// # 一对多的方向在这里翻了个身
///
/// 源表是 1 简 → N 繁（`出 → 出 齣`），反转后**多个繁体映到同一简体**是常态，而
/// **同一繁体来自多个简体**才是要处理的冲突（如 `幹`/`乾` 都可能反查到不同简体源）。
/// [`write_octrie`] 按 key 排序后 `dedup_by` 保留首次出现者，而 `sort_by` 是稳定排序
/// ⇒ 胜出的是**源文件里靠前**的那条。这与主表 `compile_octrie` 取多值首项的
/// 「定义序即优先级」是同一条约定。
///
/// ⚠️ 产物名刻意**不叫** `TSCharacters`：OpenCC 上游确有同名文件，若哪天它被下载进
/// `.cache/opencc/dictionaries/`，主循环会编译出同名 `.octrie` 而与本表互相覆盖
/// （`STVariants` 已有这个隐患，不要再添一个）。
fn compile_t2s_octrie(src: &Path, dst: &Path) -> anyhow::Result<usize> {
    let content = std::fs::read_to_string(src)?;
    let mut pairs: Vec<(Vec<u8>, Vec<u8>)> = Vec::new();

    for line in content.lines() {
        let Some((simp, rest)) = parse_line(line) else {
            continue;
        };
        for trad in rest.split(' ').filter(|v| !v.is_empty()) {
            // 繁简同形（`人 → 人`）不入表：查不到即原样返回，留着只是白占空间。
            if trad == simp {
                continue;
            }
            pairs.push((trad.as_bytes().to_vec(), simp.as_bytes().to_vec()));
        }
    }
    write_octrie(pairs, dst)
}

/// 解析一行 TSV：跳过空行/注释/无 tab 行，返回 (key, 值区原串)。
fn parse_line(line: &str) -> Option<(&str, &str)> {
    let line = line.trim();
    if line.is_empty() || line.starts_with('#') {
        return None;
    }
    let tab = line.find('\t')?;
    let key = &line[..tab];
    if key.is_empty() {
        return None;
    }
    Some((key, &line[tab + 1..]))
}

fn write_octrie(mut pairs: Vec<(Vec<u8>, Vec<u8>)>, dst: &Path) -> anyhow::Result<usize> {
    // 按 key 字节序排序，去重（保留首次出现）
    pairs.sort_by(|a, b| a.0.cmp(&b.0));
    pairs.dedup_by(|a, b| a.0 == b.0);

    let count = pairs.len();
    let max_key_bytes = pairs.iter().map(|(k, _)| k.len()).max().unwrap_or(0) as u16;

    // StringPool：先顺序写所有 key，再顺序写所有 val
    let mut pool: Vec<u8> = Vec::new();
    let mut key_segs: Vec<(u32, u16)> = Vec::with_capacity(count);
    for (key, _) in &pairs {
        key_segs.push((pool.len() as u32, key.len() as u16));
        pool.extend_from_slice(key);
    }
    let mut val_segs: Vec<(u32, u16)> = Vec::with_capacity(count);
    for (_, val) in &pairs {
        val_segs.push((pool.len() as u32, val.len() as u16));
        pool.extend_from_slice(val);
    }

    let tmp = dst.with_extension("octrie.tmp");
    {
        let mut f = std::fs::File::create(&tmp)?;
        // Header (16B)
        f.write_all(b"WIOC")?;
        f.write_all(&1u32.to_le_bytes())?;
        f.write_all(&(count as u32).to_le_bytes())?;
        f.write_all(&max_key_bytes.to_le_bytes())?;
        f.write_all(&0u16.to_le_bytes())?;
        // Entries (N × 12B)
        for i in 0..count {
            f.write_all(&key_segs[i].0.to_le_bytes())?;
            f.write_all(&key_segs[i].1.to_le_bytes())?;
            f.write_all(&val_segs[i].0.to_le_bytes())?;
            f.write_all(&val_segs[i].1.to_le_bytes())?;
        }
        // StringPool
        f.write_all(&pool)?;
    }
    std::fs::rename(&tmp, dst)?;
    Ok(count)
}

fn parse_args(args: &[String]) -> anyhow::Result<(PathBuf, PathBuf)> {
    let mut src = None;
    let mut out = None;
    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--src" | "-src" => {
                i += 1;
                src = args.get(i).map(PathBuf::from);
            }
            "--out" | "-out" => {
                i += 1;
                out = args.get(i).map(PathBuf::from);
            }
            _ => {}
        }
        i += 1;
    }
    Ok((
        src.ok_or_else(|| anyhow::anyhow!("用法: gen_opencc --src <dir> --out <dir>"))?,
        out.ok_or_else(|| anyhow::anyhow!("用法: gen_opencc --src <dir> --out <dir>"))?,
    ))
}
