//! 文本目标的文件层（设计 §3.5 第 1 步的读、写、复核）。引擎侧的折回 / 同步 / 整文件替换在
//! `engine.rs`（要动视图与 state），这里只管文件：指纹、读、带复核的提交。

use std::path::{Path, PathBuf};
use std::sync::Mutex;

type Hook = Box<dyn Fn() + Send + Sync>;

static BEFORE_RECHECK: Mutex<Vec<(u64, std::sync::Arc<Hook>)>> = Mutex::new(Vec::new());
static NEXT_HOOK_ID: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// 测试钩子：提交时「tmp 已写好、复核原文件指纹之前」调用——借此模拟编辑器恰在折回期间保存。
/// 进程级、可并存（并发的测试各装各的，钩子里自己按线程过滤）；守卫 drop 时只卸下这一个。
#[doc(hidden)]
pub fn add_before_rename_hook(h: Hook) -> HookGuard {
    let id = NEXT_HOOK_ID.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    BEFORE_RECHECK
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .push((id, std::sync::Arc::new(h)));
    HookGuard(id)
}

#[doc(hidden)]
pub struct HookGuard(u64);

impl Drop for HookGuard {
    fn drop(&mut self) {
        BEFORE_RECHECK
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .retain(|(id, _)| *id != self.0);
    }
}

/// 内容指纹：`"{len:x}-{crc32:08x}"`；文件不存在为 `"absent"`。
pub fn fingerprint(bytes: Option<&[u8]>) -> String {
    match bytes {
        Some(b) => format!("{:x}-{:08x}", b.len(), crc32fast::hash(b)),
        None => "absent".to_string(),
    }
}

/// 读文件：不存在 ⇒ `Ok(None)`；其他错误（权限、被占用）⇒ `Err`（调用方本轮跳过，不当成删除）。
pub fn read_text(path: &Path) -> anyhow::Result<Option<(Vec<u8>, i64)>> {
    match std::fs::read(path) {
        Ok(b) => {
            let mtime = std::fs::metadata(path)
                .and_then(|m| m.modified())
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map_or(0, |d| d.as_secs() as i64);
            Ok(Some((b, mtime)))
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(anyhow::anyhow!("读不了 {}：{e}", path.display())),
    }
}

fn with_suffix(path: &Path, suffix: &str) -> PathBuf {
    let mut s = path.as_os_str().to_os_string();
    s.push(suffix);
    PathBuf::from(s)
}

/// 数据目录下放上一版与坏文件留存的子目录名（按数据目录的相对路径原样排布）。
pub const BACKUP_DIR: &str = ".backup";
/// 每个原文件最多留这么多份坏文件留存（多出的删最旧的）。
pub const MAX_CORRUPT_KEPT: usize = 5;

/// 已登记的数据目录（store 打开时登记；按路径前缀认领其下的文件）。进程级：测试各用各的临时目录，
/// 互不认领。
static DATA_ROOTS: Mutex<Vec<PathBuf>> = Mutex::new(Vec::new());

/// 登记数据目录：之后其下文件的上一版与坏文件留存都放进 `<root>/.backup/`。重复登记无害。
pub fn register_data_root(root: &Path) {
    let mut g = DATA_ROOTS.lock().unwrap_or_else(|e| e.into_inner());
    if !g.iter().any(|r| r == root) {
        g.push(root.to_path_buf());
    }
}

/// `path` 所在的已登记数据目录与相对路径（取最长的那个；`.backup/` 里的文件不算）。
fn data_rel(path: &Path) -> Option<(PathBuf, PathBuf)> {
    let g = DATA_ROOTS.lock().unwrap_or_else(|e| e.into_inner());
    g.iter()
        .filter_map(|r| Some((r, path.strip_prefix(r).ok()?)))
        .filter(|(_, rel)| {
            rel.components()
                .next()
                .is_some_and(|c| c.as_os_str() != BACKUP_DIR)
        })
        .max_by_key(|(r, _)| r.components().count())
        .map(|(r, rel)| (r.clone(), rel.to_path_buf()))
}

/// 提交前的上一版放在哪：数据目录下 ⇒ `<root>/.backup/<相对路径>`；否则同目录 `<文件>.bak`。
pub fn backup_path(path: &Path) -> PathBuf {
    match data_rel(path) {
        Some((root, rel)) => root.join(BACKUP_DIR).join(rel),
        None => legacy_backup_path(path),
    }
}

/// 旧版的同目录备份 `<文件>.bak`（只供恢复时回退读取与迁移）。
pub fn legacy_backup_path(path: &Path) -> PathBuf {
    with_suffix(path, ".bak")
}

/// 文件删掉时一并删它的上一版（新旧两处位置）。
pub fn remove_backup(path: &Path) {
    for p in [backup_path(path), legacy_backup_path(path)] {
        let _ = std::fs::remove_file(p);
    }
}

/// 文件改名时把它的上一版挪到新名字下（没有就什么都不做；挪不动只 WARN）。
pub fn move_backup(from: &Path, to: &Path) {
    let (src, dest) = (backup_path(from), backup_path(to));
    let src = if src.is_file() {
        src
    } else {
        legacy_backup_path(from)
    };
    if !src.is_file() {
        return;
    }
    if let Some(d) = dest.parent() {
        let _ = std::fs::create_dir_all(d);
    }
    if let Err(e) = std::fs::rename(&src, &dest) {
        tracing::warn!("备份 {} 没能随文件改名：{e}", src.display());
    }
}

/// 给用户看的留存位置：数据目录下 ⇒ 相对数据目录、`/` 分隔；否则文件名。
pub fn kept_as(kept: &Path) -> String {
    let g = DATA_ROOTS.lock().unwrap_or_else(|e| e.into_inner());
    match g
        .iter()
        .filter_map(|r| kept.strip_prefix(r).ok())
        .min_by_key(|rel| rel.components().count())
    {
        Some(rel) => rel
            .components()
            .map(|c| c.as_os_str().to_string_lossy().into_owned())
            .collect::<Vec<_>>()
            .join("/"),
        None => kept
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default(),
    }
}

/// 留存名 `<name>.corrupt-<YYYYMMDD-HHMMSS>[-N]` 的排序键（时间, N）；不是这个形状 ⇒ None。
fn corrupt_key(file: &str, name: &str) -> Option<(String, u32)> {
    let rest = file.strip_prefix(name)?.strip_prefix(".corrupt-")?;
    let (stamp, n) = match rest.get(15..) {
        Some("") => (rest, 1),
        Some(tail) => (&rest[..15], tail.strip_prefix('-')?.parse().ok()?),
        None => return None,
    };
    let b = stamp.as_bytes();
    let ok = b.len() == 15
        && b[8] == b'-'
        && b.iter()
            .enumerate()
            .all(|(i, c)| i == 8 || c.is_ascii_digit());
    ok.then(|| (stamp.to_string(), n))
}

/// `dir` 里 `name` 的坏文件留存只留最新的 [`MAX_CORRUPT_KEPT`] 份。
fn prune_corrupt(dir: &Path, name: &str) {
    let mut kept: Vec<((String, u32), PathBuf)> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| Some((corrupt_key(e.file_name().to_str()?, name)?, e.path())))
        .collect();
    if kept.len() <= MAX_CORRUPT_KEPT {
        return;
    }
    kept.sort();
    for (_, p) in &kept[..kept.len() - MAX_CORRUPT_KEPT] {
        let _ = std::fs::remove_file(p);
    }
}

/// 在 `dir` 里给 `base` 取个不重名的名字（重名加 `-2`、`-3`…）。
fn unique(base: PathBuf) -> PathBuf {
    let mut dest = base.clone();
    let mut n = 2;
    while dest.exists() {
        dest = with_suffix(&base, &format!("-{n}"));
        n += 1;
    }
    dest
}

/// `.backup/` 里的标记文件：旧版备份已迁移过，之后不再扫。
pub const MIGRATED_MARK: &str = ".migrated";

/// `dir` 里 `name` 的留存名：`<name>.corrupt-<stamp>`；同一时间已有 ⇒ 接着已有的最大号续
/// `-N`（最旧的一份刚被清掉时也不能拿回它的名字，否则新留存排在最前、下一次清理就把它删了）。
fn next_corrupt_name(dir: &Path, name: &str, stamp: &str) -> String {
    let last_n = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| corrupt_key(e.file_name().to_str()?, name))
        .filter(|(s, _)| *s == stamp)
        .map(|(_, n)| n)
        .max();
    match last_n {
        None => format!("{name}.corrupt-{stamp}"),
        Some(n) => format!("{name}.corrupt-{stamp}-{}", n + 1),
    }
}

fn mtime(p: &Path) -> Option<std::time::SystemTime> {
    std::fs::metadata(p).and_then(|m| m.modified()).ok()
}

/// 旁边的 `src`（`.bak`）挪到 `dest`。`dest` 已有 ⇒ 不删任何一份：`src` 更新就把 `dest` 另存为
/// `<dest>.older[-N]` 再挪；否则 `src` 留在原处。挪了返回 true。
fn migrate_bak(src: &Path, dest: &Path) -> std::io::Result<bool> {
    if dest.exists() {
        if mtime(src) <= mtime(dest) {
            return Ok(false);
        }
        std::fs::rename(dest, unique(with_suffix(dest, ".older")))?;
    }
    std::fs::rename(src, dest)?;
    Ok(true)
}

/// 首次启动时把旧版留在数据文件旁边的 `<文件>.bak` 与 `<文件>.corrupt-<时间>` 挪进
/// `<root>/.backup/`（相对路径不变）。只认程序的数据文件（`is_data_file` 按相对路径分量判）：`.bak`
/// 还要原文件在（原文件不在的多半是用户自己的）；坏文件留存名是程序生成的形状，原文件不要求在。
/// 不删任何文件、不按份数裁剪；跑完在 `.backup/` 放 [`MIGRATED_MARK`]，之后不再扫——用户之后自己
/// 放的 `.bak` 不会被收走。返回挪动的个数。
pub fn migrate_legacy_backups(root: &Path, is_data_file: impl Fn(&[&str]) -> bool) -> usize {
    let mark = root.join(BACKUP_DIR).join(MIGRATED_MARK);
    if mark.exists() {
        return 0;
    }
    let mut moved = 0;
    let mut stack = vec![root.to_path_buf()];
    while let Some(d) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&d) else {
            continue;
        };
        for e in rd.flatten() {
            let p = e.path();
            let Some(file) = e.file_name().to_str().map(str::to_string) else {
                continue;
            };
            if e.file_type().is_ok_and(|t| t.is_dir()) {
                if !(d == root && file == BACKUP_DIR) {
                    stack.push(p);
                }
                continue;
            }
            let Ok(rel_dir) = d.strip_prefix(root) else {
                continue;
            };
            let rel_of = |orig: &str| -> Vec<String> {
                rel_dir
                    .components()
                    .map(|c| c.as_os_str().to_string_lossy().into_owned())
                    .chain([orig.to_string()])
                    .collect()
            };
            let is_data = |orig: &str| {
                let rel = rel_of(orig);
                is_data_file(&rel.iter().map(String::as_str).collect::<Vec<_>>())
            };
            let dest_dir = root.join(BACKUP_DIR).join(rel_dir);
            let r = if let Some(orig) = file.strip_suffix(".bak")
                && is_data(orig)
                && d.join(orig).is_file()
            {
                std::fs::create_dir_all(&dest_dir)
                    .and_then(|()| migrate_bak(&p, &dest_dir.join(orig)))
            } else if let Some(i) = file.find(".corrupt-")
                && let Some((stamp, _)) = corrupt_key(&file, &file[..i])
                && is_data(&file[..i])
            {
                std::fs::create_dir_all(&dest_dir).and_then(|()| {
                    let mut dest = dest_dir.join(&file);
                    if dest.exists() {
                        dest = dest_dir.join(next_corrupt_name(&dest_dir, &file[..i], &stamp));
                    }
                    std::fs::rename(&p, unique(dest)).map(|()| true)
                })
            } else {
                continue;
            };
            match r {
                Ok(true) => moved += 1,
                Ok(false) => {}
                Err(e) => tracing::warn!("旧备份 {} 没能挪进 {BACKUP_DIR}：{e}", p.display()),
            }
        }
    }
    if let Err(e) =
        std::fs::create_dir_all(root.join(BACKUP_DIR)).and_then(|()| std::fs::write(&mark, b""))
    {
        tracing::warn!("写不了 {}：{e}", mark.display());
    }
    if moved > 0 {
        tracing::info!("已把 {moved} 个旧备份 / 坏文件留存挪进 {BACKUP_DIR}");
    }
    moved
}

/// UTC `YYYYMMDD-HHMMSS`（不引 chrono：civil-from-days 换算）。
fn utc_stamp() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64);
    let (days, rem) = (secs.div_euclid(86_400), secs.rem_euclid(86_400));
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!(
        "{y:04}{m:02}{d:02}-{:02}{:02}{:02}",
        rem / 3600,
        rem % 3600 / 60,
        rem % 60
    )
}

/// 把解析不了的文件改名留存为 `<文件名>.corrupt-<UTC 时间>`（设计 §2.5：坏文件不被覆盖）：数据目录下
/// 的放进 `<root>/.backup/<相对目录>/`，否则留在原目录。每个原文件只留最新的
/// [`MAX_CORRUPT_KEPT`] 份。文件不存在 ⇒ `Ok(None)`；同一秒已有同名留存 ⇒ 名字后加 `-2`、`-3`…
pub fn preserve_corrupt(path: &Path) -> std::io::Result<Option<PathBuf>> {
    if !path.exists() {
        return Ok(None);
    }
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let dir = match data_rel(path) {
        Some((root, rel)) => {
            let d = root
                .join(BACKUP_DIR)
                .join(rel.parent().unwrap_or(Path::new("")));
            std::fs::create_dir_all(&d)?;
            d
        }
        None => path.parent().unwrap_or(Path::new("")).to_path_buf(),
    };
    let dest = unique(dir.join(next_corrupt_name(&dir, &name, &utc_stamp())));
    std::fs::rename(path, &dest)?;
    tracing::warn!("{} 无法解析，已另存为 {}", path.display(), dest.display());
    prune_corrupt(&dir, &name);
    Ok(Some(dest))
}

/// §3.5 第 1 步的写回：tmp → fsync → （`expect_fp` 给出时）复核原文件指纹未变 → 原文件存在则留
/// 上一版（[`backup_path`]）→ rename → fsync 目录（Windows 对目标再 `sync_all`）。复核不过 ⇒ 删 tmp、`Err`。
pub fn commit_text(path: &Path, bytes: &[u8], expect_fp: Option<&str>) -> anyhow::Result<()> {
    let dir = path
        .parent()
        .ok_or_else(|| anyhow::anyhow!("无父目录: {}", path.display()))?;
    std::fs::create_dir_all(dir)?;
    let tmp = with_suffix(path, ".tmp");
    {
        let mut f = std::fs::File::create(&tmp)?;
        std::io::Write::write_all(&mut f, bytes)?;
        f.sync_all()?;
    }
    // 拷出来再调：钩子可能阻塞（模拟慢 fsync），别让它占着锁卡住别的测试的提交
    let hooks: Vec<_> = BEFORE_RECHECK
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .iter()
        .map(|(_, h)| h.clone())
        .collect();
    for h in hooks {
        h();
    }
    if let Some(fp) = expect_fp {
        let now = read_text(path)?;
        if fingerprint(now.as_ref().map(|(b, _)| b.as_slice())) != fp {
            let _ = std::fs::remove_file(&tmp);
            anyhow::bail!("{} 在折回期间被修改，本轮放弃", path.display());
        }
    }
    if path.is_file() {
        // 上一版只是保险：写不出来（`.backup/` 被占、磁盘权限）不拦数据文件本身的提交
        let bak = backup_path(path);
        if let Err(e) = bak
            .parent()
            .map_or(Ok(()), std::fs::create_dir_all)
            .and_then(|()| std::fs::copy(path, &bak).map(|_| ()))
        {
            tracing::warn!("{} 的上一版没能留存：{e}", path.display());
        }
    }
    if let Err(e) = std::fs::rename(&tmp, path) {
        let _ = std::fs::remove_file(&tmp);
        return Err(e.into());
    }
    #[cfg(unix)]
    {
        std::fs::File::open(dir)?.sync_all()?;
    }
    #[cfg(not(unix))]
    {
        let _ = crate::state::fsync_file(path);
    }
    Ok(())
}

#[cfg(test)]
#[path = "fold_tests.rs"]
mod tests;
