//! 用户分库（设计 `user-dict-libraries-and-external-edit.md` §3）：`<数据方案>/dicts/*.dict.yaml`
//! 一个文件一个分库（不递归），开关在 `<数据方案>/dicts.toml`，按方案记关掉的分库名。
//! 引擎把分库登记成方案的扩展词库（id `user:<名>`），这里只管文件。

use crate::store::Store;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use wind_userdata::issues::{self, IssueKind};
use wind_userdata::text::{banner, fold};

/// 分库目录名（相对数据方案目录）。
pub const LIBS_DIR: &str = "dicts";
/// 开关文件名（相对数据方案目录）。
pub const META_FILE: &str = "dicts.toml";
/// 分库 id 前缀（方案词库表里）。
pub const ID_PREFIX: &str = "user:";
const SUFFIX: &str = ".dict.yaml";
/// 拼音族的数据方案：分库码按音节空格分隔（同 user.dict.yaml）。
const PINYIN_DS: &str = "pinyin";

/// 分库名合法性：非空、≤64 字符、不含 `/ \ : * ? " < > |` 与控制字符、不以 `.` 开头；
/// 首尾不能是空白、不以 `.` 结尾（Windows 会悄悄去掉它们，文件名就和分库名对不上）；
/// 不能是 Windows 保留设备名；文件名（含 `.dict.yaml`）不超过 255 字节。
pub fn validate_name(name: &str) -> anyhow::Result<()> {
    if is_windows_reserved(name) {
        anyhow::bail!("分库名不合法：{name:?} 是 Windows 保留的设备名");
    }
    if name.len() + SUFFIX.len() > 255 {
        anyhow::bail!("分库名太长：{name:?}");
    }
    let bad = name.is_empty()
        || name.chars().count() > 64
        || name
            .chars()
            .any(|c| c.is_control() || "/\\:*?\"<>|".contains(c))
        || name.starts_with('.')
        || name.ends_with('.')
        || name.starts_with(char::is_whitespace)
        || name.ends_with(char::is_whitespace);
    if bad {
        anyhow::bail!(
            "分库名不合法：{name:?}（不能为空、最多 64 个字，不能含 / \\ : * ? \" < > |，不能以 . 开头或结尾，首尾不能是空格）"
        );
    }
    Ok(())
}

/// 分库文件名 → 分库名（去掉 `.dict.yaml`）；不是分库文件名为 None。`fold_case`：后缀不分
/// 大小写（Windows）。
pub(crate) fn lib_name(file: &str, fold_case: bool) -> Option<&str> {
    let cut = file.len().checked_sub(SUFFIX.len())?;
    let (name, suffix) = (file.get(..cut)?, file.get(cut..)?);
    (suffix == SUFFIX || (fold_case && suffix.eq_ignore_ascii_case(SUFFIX))).then_some(name)
}

/// Windows 保留设备名（CON、PRN、AUX、NUL、COM1–9、LPT1–9）：按 Windows 的规则取第一个 `.`
/// 之前、去掉末尾空格的部分，不分大小写比较。
fn is_windows_reserved(name: &str) -> bool {
    let stem = name
        .split('.')
        .next()
        .unwrap_or("")
        .trim_end_matches(' ')
        .to_ascii_uppercase();
    match stem.as_str() {
        "CON" | "PRN" | "AUX" | "NUL" => true,
        s => {
            (s.starts_with("COM") || s.starts_with("LPT"))
                && s.len() == 4
                && matches!(s.as_bytes()[3], b'1'..=b'9')
        }
    }
}

pub fn libs_dir(text_dir: &Path, ds: &str) -> PathBuf {
    text_dir.join(ds).join(LIBS_DIR)
}

pub fn lib_path(text_dir: &Path, ds: &str, name: &str) -> PathBuf {
    libs_dir(text_dir, ds).join(format!("{name}{SUFFIX}"))
}

pub fn meta_path(text_dir: &Path, ds: &str) -> PathBuf {
    text_dir.join(ds).join(META_FILE)
}

/// 目录下的一个分库文件。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LibFile {
    pub name: String,
    pub path: PathBuf,
}

/// 列出 `<ds>/dicts/` 下的分库（不递归、只认 `*.dict.yaml`、名字不合法的跳过），按名排序。
pub fn scan(text_dir: &Path, ds: &str) -> Vec<LibFile> {
    let Ok(rd) = std::fs::read_dir(libs_dir(text_dir, ds)) else {
        return Vec::new();
    };
    let mut out: Vec<LibFile> = rd
        .flatten()
        .filter_map(|e| {
            let path = e.path();
            let file = e.file_name().to_str()?.to_string();
            let name = file.strip_suffix(SUFFIX)?;
            (path.is_file() && validate_name(name).is_ok()).then(|| LibFile {
                name: name.to_string(),
                path,
            })
        })
        .collect();
    out.sort_by(|a, b| a.name.cmp(&b.name));
    out
}

/// 词库头的 `name:`（显示名）；没有 / 读不了为空串。
pub fn read_label(path: &Path) -> String {
    use std::io::Read;
    // 只读头部：分库正文可能很大，`name:` 在前几行
    let mut buf = Vec::new();
    let Ok(f) = std::fs::File::open(path) else {
        return String::new();
    };
    if f.take(64 * 1024).read_to_end(&mut buf).is_err() {
        return String::new();
    }
    let raw = String::from_utf8_lossy(&buf);
    let text = wind_utils::text::normalize_input(&raw);
    let mut head = String::new();
    for line in text.lines() {
        if line.trim_end() == "..." {
            break;
        }
        head.push_str(line.trim_start_matches('\u{feff}'));
        head.push('\n');
    }
    serde_yaml::from_str::<serde_yaml::Value>(&head)
        .ok()
        .and_then(|v| v.get("name").and_then(|n| n.as_str()).map(str::to_string))
        .map(|n| n.trim().to_string())
        .unwrap_or_default()
}

/// 进程内上次成功读到的开关文件内容（路径 → 方案 id → 关掉的分库名）。
type LastMeta = std::collections::HashMap<PathBuf, BTreeMap<String, Vec<String>>>;
static LAST_META: std::sync::LazyLock<std::sync::Mutex<LastMeta>> =
    std::sync::LazyLock::new(Default::default);

/// 该方案关掉的分库名。开关文件格式坏 ⇒ 按全开，并登记到 `storageStatus.issues`；读不了
/// （IO 错误，多半是一时被占用）⇒ 沿用进程内上次读到的开关（从没读到过就全开）。
pub fn off_names(text_dir: &Path, ds: &str, schema_id: &str) -> Vec<String> {
    let p = meta_path(text_dir, ds);
    let isolated = || {
        issues::list_under(&p)
            .iter()
            .any(|i| i.path == p && i.kind == IssueKind::Isolated)
    };
    let mut last = LAST_META.lock().unwrap_or_else(|e| e.into_inner());
    match load_meta(&p) {
        Ok((Some(m), _)) => {
            if isolated() {
                issues::clear(&p);
            }
            let switches: BTreeMap<String, Vec<String>> =
                m.schemas.into_iter().map(|(k, v)| (k, v.off)).collect();
            let off = switches.get(schema_id).cloned().unwrap_or_default();
            last.insert(p, switches);
            off
        }
        Err(e) => {
            tracing::warn!("{} 读取失败，用户分库沿用上次的开关：{e:#}", p.display());
            last.get(&p)
                .and_then(|m| m.get(schema_id))
                .cloned()
                .unwrap_or_default()
        }
        Ok((None, _)) => {
            if !isolated() {
                tracing::warn!("{} 无法读取，用户分库按全开处理", p.display());
                issues::report(&p, IssueKind::Isolated);
            }
            Vec::new()
        }
    }
}

/// 该数据方案全部分库与开关文件的内容指纹（`dict.reload` 不带路径时据此判断变没变）。
pub fn fingerprint(text_dir: &Path, ds: &str) -> String {
    let mut s = file_fingerprint(&meta_path(text_dir, ds));
    for l in scan(text_dir, ds) {
        s.push_str(&format!("|{}={}", l.name, file_fingerprint(&l.path)));
    }
    s
}

/// 单个分库 / 开关文件的内容指纹（读不了为 `err`）。
pub(crate) fn file_fingerprint(p: &Path) -> String {
    match fold::read_text(p) {
        Ok(b) => fold::fingerprint(b.as_ref().map(|(b, _)| b.as_slice())),
        Err(_) => "err".to_string(),
    }
}

/// 重新解析一个分库 / 开关文件并登记（或清掉）它的问题；文件不在了只清。
pub(crate) fn report_file_issues(p: &Path) {
    if !p.exists() {
        issues::clear(p);
        return;
    }
    if p.file_name().is_some_and(|n| n == META_FILE) {
        let isolated = issues::list_under(p)
            .iter()
            .any(|i| i.path == p && i.kind == IssueKind::Isolated);
        match load_meta(p) {
            Ok((Some(_), _)) if isolated => issues::clear(p),
            Ok((Some(_), _)) => {}
            Ok((None, _)) | Err(_) => issues::report(p, IssueKind::Isolated),
        }
        return;
    }
    match parse_lib(p) {
        Ok(d) => issues::report_parse(p, d.bad_lines),
        Err(_) => issues::report(p, IssueKind::Isolated),
    }
}

/// 读开关文件：不存在 ⇒ 空表；格式坏 ⇒ `None`；读不了 ⇒ Err。另返回读到的内容指纹。
fn load_meta(path: &Path) -> anyhow::Result<(Option<Meta>, String)> {
    let Some((b, _)) = fold::read_text(path)? else {
        return Ok((Some(Meta::default()), fold::fingerprint(None)));
    };
    let fp = fold::fingerprint(Some(&b));
    let meta = std::str::from_utf8(&b)
        .ok()
        .and_then(|s| toml::from_str::<Meta>(s).ok());
    Ok((meta, fp))
}

/// 文件头里的显示名写成双引号串：名字里不会有 `"` 和 `\`（[`validate_name`]），其余字符都安全。
fn quoted(name: &str) -> String {
    format!("\"{name}\"")
}

/// `dicts.toml` 的内容：方案 id → 关掉的分库名。
#[derive(Default, serde::Serialize, serde::Deserialize)]
struct Meta {
    #[serde(default)]
    schemas: BTreeMap<String, Switches>,
}

#[derive(Default, serde::Serialize, serde::Deserialize)]
struct Switches {
    #[serde(default)]
    off: Vec<String>,
}

/// `userDicts.list` 的一项。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LibInfo {
    pub name: String,
    pub label: String,
    pub enabled: bool,
    /// 有效词条数。
    pub entries: usize,
    /// 相对数据目录、`/` 分隔。
    pub file: String,
    /// 解析不了而跳过的行数。
    pub bad_lines: usize,
    /// 整份解析不了（引擎照样按能读的部分加载，这里只报）。
    pub unreadable: bool,
}

impl Store {
    /// 该数据方案下的分库及其在 `schema_id` 下的开关。
    pub fn list_user_dicts(&self, ds: &str, schema_id: &str) -> anyhow::Result<Vec<LibInfo>> {
        wind_userdata::engine::check_schema_id(ds)?;
        let root = self.user_text_dir();
        let off = off_names(root, ds, schema_id);
        Ok(scan(root, ds)
            .into_iter()
            .map(|l| {
                let (entries, bad_lines, unreadable) = match parse_lib(&l.path) {
                    Ok(d) => {
                        issues::report_parse(&l.path, d.bad_lines);
                        (d.entries.len(), d.bad_lines, false)
                    }
                    Err(_) => {
                        issues::report(&l.path, IssueKind::Isolated);
                        (0, 0, true)
                    }
                };
                let label = read_label(&l.path);
                LibInfo {
                    label: if label.is_empty() {
                        l.name.clone()
                    } else {
                        label
                    },
                    enabled: !off.contains(&l.name),
                    entries,
                    file: format!("{ds}/{LIBS_DIR}/{}{SUFFIX}", l.name),
                    bad_lines,
                    unreadable,
                    name: l.name,
                }
            })
            .collect())
    }

    fn ensure_writable(&self) -> anyhow::Result<()> {
        if self.runtime().data().readonly() {
            anyhow::bail!("只读模式：本次运行不保存修改");
        }
        Ok(())
    }

    /// 已有分库的路径（校验名字与数据方案）；不存在报错。
    fn existing_lib(&self, ds: &str, name: &str) -> anyhow::Result<PathBuf> {
        wind_userdata::engine::check_schema_id(ds)?;
        validate_name(name)?;
        let p = lib_path(self.user_text_dir(), ds, name);
        if !p.is_file() {
            anyhow::bail!("分库不存在：{name}");
        }
        Ok(p)
    }

    /// 新分库的路径（校验名字与数据方案）；已存在报错。
    fn new_lib(&self, ds: &str, name: &str) -> anyhow::Result<PathBuf> {
        wind_userdata::engine::check_schema_id(ds)?;
        validate_name(name)?;
        let p = lib_path(self.user_text_dir(), ds, name);
        if p.exists() {
            anyhow::bail!("分库已存在：{name}");
        }
        Ok(p)
    }

    /// 改开关文件：`f` 返回 false（没改）就不写。格式坏的文件先另存留存再从空表改。
    fn modify_meta(&self, ds: &str, f: impl FnOnce(&mut Meta) -> bool) -> anyhow::Result<()> {
        let p = meta_path(self.user_text_dir(), ds);
        self.check_not_editing(&p)?;
        let (loaded, mut fp) = load_meta(&p)?;
        let corrupt = loaded.is_none();
        let mut meta = loaded.unwrap_or_default();
        if !f(&mut meta) {
            return Ok(());
        }
        if corrupt && let Some(kept) = fold::preserve_corrupt(&p)? {
            let kept_as = fold::kept_as(&kept);
            issues::report(&p, IssueKind::Rebuilt { kept_as });
            fp = fold::fingerprint(None);
        }
        for sw in meta.schemas.values_mut() {
            sw.off.sort();
            sw.off.dedup();
        }
        meta.schemas.retain(|_, sw| !sw.off.is_empty());
        let text = format!("{}\n{}", banner::USER_DICTS_META, toml::to_string(&meta)?);
        fold::commit_text(&p, text.as_bytes(), Some(&fp))
    }

    /// 某分库的全部有效词条（码为扁平码 + 音节边界）。
    pub fn user_dict_entries(
        &self,
        ds: &str,
        name: &str,
    ) -> anyhow::Result<Vec<wind_userdata::text::TextEntry>> {
        let p = self.existing_lib(ds, name)?;
        Ok(parse_lib(&p)?.entries)
    }

    /// 改某方案下某分库的开关（写 `dicts.toml`）。
    pub fn set_user_dict_enabled(
        &self,
        ds: &str,
        schema_id: &str,
        name: &str,
        enabled: bool,
    ) -> anyhow::Result<()> {
        self.ensure_writable()?;
        wind_userdata::engine::check_schema_id(ds)?;
        validate_name(name)?;
        self.modify_meta(ds, |m| {
            let sw = m.schemas.entry(schema_id.to_string()).or_default();
            let was_off = sw.off.iter().any(|n| n == name);
            if enabled {
                sw.off.retain(|n| n != name);
            } else if !was_off {
                sw.off.push(name.to_string());
            }
            was_off == enabled
        })
    }

    /// 新建空分库（说明行 + 与 user.dict.yaml 相同的文件头）；已存在报错。
    pub fn create_user_dict(&self, ds: &str, name: &str) -> anyhow::Result<PathBuf> {
        self.write_new_lib(ds, name, &[])
    }

    fn write_new_lib(
        &self,
        ds: &str,
        name: &str,
        rows: &[(String, String, u64, i32)],
    ) -> anyhow::Result<PathBuf> {
        self.ensure_writable()?;
        let p = self.new_lib(ds, name)?;
        let text = wind_userdata::text::render_with(
            banner::USER_DICT_LIB,
            &quoted(name),
            ds == PINYIN_DS,
            rows,
        );
        // 期望指纹 absent：落盘前有人抢先放了同名文件就放弃，不覆盖
        fold::commit_text(&p, text.as_bytes(), Some(&fold::fingerprint(None)))?;
        Ok(p)
    }

    /// 把解析好的词条整写成新分库；已存在报错。返回写入条数（同码同词只留一条）。
    pub fn import_user_dict(
        &self,
        ds: &str,
        name: &str,
        rows: &[crate::wdict::WordIo],
    ) -> anyhow::Result<usize> {
        let mut out: Vec<(String, String, u64, i32)> = Vec::with_capacity(rows.len());
        let mut at: std::collections::HashMap<(String, String), usize> =
            std::collections::HashMap::new();
        for r in rows {
            let (flat, b, text) = wind_userdata::text::normalize_key(&r.code, &r.text, false);
            if flat.is_empty() || text.is_empty() {
                continue;
            }
            match at.get(&(flat.clone(), text.clone())) {
                Some(&i) => out[i].3 = out[i].3.max(r.weight),
                None => {
                    at.insert((flat.clone(), text.clone()), out.len());
                    out.push((flat, text, r.boundary.unwrap_or(b), r.weight));
                }
            }
        }
        self.write_new_lib(ds, name, &out)?;
        Ok(out.len())
    }

    /// 改名：文件改名，文件头 `name:` 恰为旧名时一并改；`dicts.toml` 里各方案的记录同步。
    pub fn rename_user_dict(&self, ds: &str, name: &str, new_name: &str) -> anyhow::Result<()> {
        self.ensure_writable()?;
        let from = self.existing_lib(ds, name)?;
        if name == new_name {
            return Ok(());
        }
        let to = self.new_lib(ds, new_name)?;
        self.check_not_editing(&from)?;
        self.check_not_editing(&meta_path(self.user_text_dir(), ds))?;
        std::fs::rename(&from, &to)?;
        fold::move_backup(&from, &to);
        if read_label(&to) == name {
            rewrite_head_name(&to, new_name)?;
        }
        self.modify_meta(ds, |m| {
            let mut changed = false;
            for sw in m.schemas.values_mut() {
                for n in sw.off.iter_mut().filter(|n| *n == name) {
                    *n = new_name.to_string();
                    changed = true;
                }
            }
            changed
        })
    }

    /// 删除分库文件（不进回收站）并从 `dicts.toml` 移除。
    pub fn delete_user_dict(&self, ds: &str, name: &str) -> anyhow::Result<()> {
        self.ensure_writable()?;
        let p = self.existing_lib(ds, name)?;
        self.check_not_editing(&p)?;
        self.check_not_editing(&meta_path(self.user_text_dir(), ds))?;
        std::fs::remove_file(&p)?;
        fold::remove_backup(&p);
        issues::clear(&p);
        self.modify_meta(ds, |m| {
            let mut changed = false;
            for sw in m.schemas.values_mut() {
                let before = sw.off.len();
                sw.off.retain(|n| n != name);
                changed |= sw.off.len() != before;
            }
            changed
        })
    }

    /// 按行补丁删一条（指纹复核 + 原子替换）。没有这一条 ⇒ `Ok(false)`。
    pub fn remove_user_dict_entry(
        &self,
        ds: &str,
        name: &str,
        code: &str,
        text: &str,
    ) -> anyhow::Result<bool> {
        self.ensure_writable()?;
        let p = self.existing_lib(ds, name)?;
        self.check_not_editing(&p)?;
        let Some((bytes, mtime)) = fold::read_text(&p)? else {
            anyhow::bail!("分库不存在：{name}");
        };
        let op = wind_userdata::text::PatchOp::Delete {
            code: code.to_string(),
            text: text.to_string(),
        };
        let out = wind_userdata::text::patch_with_banner(
            &bytes,
            &p,
            false,
            &[op],
            mtime,
            name,
            ds == PINYIN_DS,
            None,
        )
        .map_err(|e| anyhow::anyhow!("分库 {name} 无法解析：{e:?}"))?;
        if out == bytes {
            return Ok(false);
        }
        fold::commit_text(&p, &out, Some(&fold::fingerprint(Some(&bytes))))?;
        Ok(true)
    }

    /// 按行补丁加一条，按 (词, 码) 去重：已有 ⇒ 改成 `weight`（没给就不动）；没有 ⇒ 追加（没给权重
    /// 时权重列留空，按方案默认）。`code` 可带音节空格；`boundary` 非 0 时优先。返回文件改没改。
    pub fn add_user_dict_entry(
        &self,
        ds: &str,
        name: &str,
        code: &str,
        text: &str,
        boundary: u64,
        weight: Option<i32>,
    ) -> anyhow::Result<bool> {
        use wind_userdata::text::PatchOp;
        self.ensure_writable()?;
        validate_entry(code, text)?;
        let p = self.existing_lib(ds, name)?;
        self.check_not_editing(&p)?;
        let Some((bytes, mtime)) = fold::read_text(&p)? else {
            anyhow::bail!("分库不存在：{name}");
        };
        let pinyin = ds == PINYIN_DS;
        let (flat, spaced, norm_text) = wind_userdata::text::normalize_key(code, text, false);
        let boundary = if boundary != 0 { boundary } else { spaced };
        let dict = wind_userdata::text::parse(&bytes, &p, false)
            .map_err(|e| anyhow::anyhow!("分库 {name} 无法解析：{e:?}"))?;
        if weight.is_some() {
            let body = std::str::from_utf8(&bytes)?;
            let body = body.strip_prefix('\u{feff}').unwrap_or(body);
            if wind_dict::rime_layout(body, &p).is_none_or(|l| l.weight_col.is_none()) {
                anyhow::bail!("分库 {name} 没有权重列，不能设权重");
            }
        }
        let existing = dict
            .entries
            .iter()
            .find(|e| e.code == flat && e.text == norm_text);
        let op = match (existing, weight) {
            (Some(_), None) => return Ok(false),
            (Some(e), Some(w)) if e.weight == w => return Ok(false),
            (Some(_), Some(w)) => PatchOp::SetWeight {
                code: flat,
                text: norm_text,
                weight: w,
                // 界面的显式修改：不受「早于文件修改时间的视为被手改覆盖」限制
                ts_secs: i64::MAX,
            },
            (None, Some(w)) => PatchOp::Upsert {
                code: flat,
                text: norm_text,
                boundary,
                weight: w,
            },
            (None, None) => PatchOp::Insert {
                code: flat,
                text: norm_text,
                boundary,
            },
        };
        let out = wind_userdata::text::patch_with_banner(
            &bytes,
            &p,
            false,
            &[op],
            mtime,
            name,
            pinyin,
            None,
        )
        .map_err(|e| anyhow::anyhow!("分库 {name} 无法解析：{e:?}"))?;
        if out == bytes {
            return Ok(false);
        }
        fold::commit_text(&p, &out, Some(&fold::fingerprint(Some(&bytes))))?;
        Ok(true)
    }

    /// 清空分库：保留文件头（含说明行）及紧跟其后、仍在注释区的注释行（`# no comment` 之后 `#`
    /// 开头的是词条），删掉全部词条。返回删掉的条数。
    pub fn clear_user_dict(&self, ds: &str, name: &str) -> anyhow::Result<usize> {
        self.ensure_writable()?;
        let p = self.existing_lib(ds, name)?;
        self.check_not_editing(&p)?;
        let Some((bytes, _)) = fold::read_text(&p)? else {
            anyhow::bail!("分库不存在：{name}");
        };
        let n = wind_userdata::text::parse(&bytes, &p, false)
            .map_err(|e| anyhow::anyhow!("分库 {name} 无法解析：{e:?}"))?
            .entries
            .len();
        let s = std::str::from_utf8(&bytes)?;
        let (bom, s) = s
            .strip_prefix('\u{feff}')
            .map_or(("", s), |rest| ("\u{feff}", rest));
        let Some((body_offset, cutoff)) = wind_dict::rime_body_layout(s) else {
            anyhow::bail!("分库 {name} 无法解析：缺少文件头结束行 ...");
        };
        let mut out = format!("{bom}{}", &s[..body_offset]);
        let mut off = body_offset;
        for seg in s[body_offset..].split_inclusive('\n') {
            let in_comment_zone = cutoff.is_none_or(|c| off <= c);
            if !(seg.starts_with('#') && in_comment_zone) {
                break;
            }
            out.push_str(seg);
            off += seg.len();
        }
        if out.as_bytes() == bytes.as_slice() {
            return Ok(0);
        }
        fold::commit_text(&p, out.as_bytes(), Some(&fold::fingerprint(Some(&bytes))))?;
        Ok(n)
    }
}

/// 界面加词的码 / 词：非空、不含 tab 与换行（一行一条、tab 分列）。
fn validate_entry(code: &str, text: &str) -> anyhow::Result<()> {
    let bad = |s: &str| s.contains(['\t', '\n', '\r']);
    if code.trim().is_empty() || text.is_empty() {
        anyhow::bail!("编码和词条都不能为空");
    }
    if bad(code) || bad(text) {
        anyhow::bail!("编码和词条里不能有制表符或换行");
    }
    Ok(())
}

fn parse_lib(p: &Path) -> anyhow::Result<wind_userdata::text::TextDict> {
    let bytes = std::fs::read(p)?;
    wind_userdata::text::parse(&bytes, p, false)
        .map_err(|e| anyhow::anyhow!("{} 无法解析：{e:?}", p.display()))
}

/// 文件头里的 `name:` 行改成新名（只改 `...` 之前那一行，其余字节不动）。
fn rewrite_head_name(path: &Path, new_name: &str) -> anyhow::Result<()> {
    let Some((b, _)) = fold::read_text(path)? else {
        return Ok(());
    };
    let s = std::str::from_utf8(&b)?;
    let mut out = String::with_capacity(s.len() + 8);
    let (mut done, mut in_head) = (false, true);
    for seg in s.split_inclusive('\n') {
        let line = seg.trim_end_matches(['\r', '\n']);
        if in_head && !done && line.starts_with("name:") {
            out.push_str(&format!("name: {}", quoted(new_name)));
            out.push_str(&seg[line.len()..]);
            done = true;
            continue;
        }
        if line == "..." {
            in_head = false;
        }
        out.push_str(seg);
    }
    if done {
        fold::commit_text(path, out.as_bytes(), Some(&fold::fingerprint(Some(&b))))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::wdict::WordIo;

    /// 开关文件读不了（IO 错误）沿用上次读到的开关；格式坏才按全开并报问题。
    #[test]
    fn off_names_keeps_last_value_when_unreadable() {
        let s = store("off_io");
        let root = s.user_text_dir().to_path_buf();
        s.create_user_dict("wb", "a").unwrap();
        s.set_user_dict_enabled("wb", "wb", "a", false).unwrap();
        assert_eq!(off_names(&root, "wb", "wb"), ["a"]);
        let meta = meta_path(&root, "wb");
        std::fs::remove_file(&meta).unwrap();
        std::fs::create_dir(&meta).unwrap();
        assert_eq!(off_names(&root, "wb", "wb"), ["a"], "读不了沿用上次");
        std::fs::remove_dir(&meta).unwrap();
        std::fs::write(&meta, "not = [valid").unwrap();
        assert!(off_names(&root, "wb", "wb").is_empty(), "格式坏按全开");
        assert!(
            issues::list_under(&meta)
                .iter()
                .any(|i| i.kind == IssueKind::Isolated)
        );
    }

    /// Windows 保留设备名（不分大小写，带扩展名也算）与超长文件名拒绝。
    #[test]
    fn validate_name_rejects_reserved_and_too_long() {
        for bad in [
            "CON", "con", "Prn", "aux", "NUL", "com1", "COM9", "lpt1", "LPT9", "con.txt",
            "Nul.x.y", "con .txt",
        ] {
            assert!(validate_name(bad).is_err(), "{bad}");
        }
        for ok in ["console", "com0", "com10", "lpt", "我的 con", "a.con"] {
            assert!(validate_name(ok).is_ok(), "{ok}");
        }
        // 64 个四字节字符：字数在限内，但文件名（含 .dict.yaml）超过 255 字节
        let emoji = "😀".repeat(64);
        assert!(validate_name(&emoji).is_err());
        assert!(validate_name(&"😀".repeat(61)).is_ok());
    }

    /// 不分大小写的文件系统（Windows）上，后缀大小写不同也认得出是分库。
    #[test]
    fn lib_name_folds_suffix_case_when_asked() {
        assert_eq!(lib_name("成语.dict.yaml", false), Some("成语"));
        assert_eq!(lib_name("成语.DICT.YAML", true), Some("成语"));
        assert_eq!(lib_name("Ab.Dict.Yaml", true), Some("Ab"));
        assert_eq!(lib_name("成语.DICT.YAML", false), None);
        assert_eq!(lib_name("dicts.toml", true), None);
    }

    fn store(tag: &str) -> Store {
        let p = std::env::temp_dir().join(format!("wind_udl_{tag}-{}.redb", std::process::id()));
        let _ = std::fs::remove_file(&p);
        let _ = std::fs::remove_dir_all(crate::store::derived_text_dir(&p));
        Store::open(&p).unwrap()
    }

    fn row(code: &str, text: &str, weight: i32) -> WordIo {
        WordIo {
            code: code.into(),
            text: text.into(),
            weight,
            count: 0,
            boundary: None,
        }
    }

    #[test]
    fn name_rules() {
        for ok in ["成语", "行业 词", "a-b_c", &"字".repeat(64)] {
            assert!(validate_name(ok).is_ok(), "{ok}");
        }
        for bad in [
            "",
            ".hidden",
            "a/b",
            "a\\b",
            "a:b",
            "a*b",
            "a?b",
            "a\"b",
            "a<b",
            "a>b",
            "a|b",
            "a\tb",
            " a",
            "a ",
            "a.",
            &"字".repeat(65),
        ] {
            assert!(validate_name(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn scan_lists_libs_sorted_non_recursive() {
        let s = store("scan");
        let root = s.user_text_dir();
        let d = libs_dir(root, "wb");
        std::fs::create_dir_all(d.join("sub")).unwrap();
        for f in [
            "b.dict.yaml",
            "a.dict.yaml",
            "c.txt",
            "sub/x.dict.yaml",
            ".h.dict.yaml",
        ] {
            std::fs::write(d.join(f), "---\n...\n").unwrap();
        }
        std::fs::write(d.join("a.dict.yaml.bak"), "").unwrap();
        let names: Vec<String> = scan(root, "wb").into_iter().map(|l| l.name).collect();
        assert_eq!(names, ["a", "b"]);
        assert!(scan(root, "none").is_empty());
    }

    fn lib_rows(s: &Store, ds: &str, name: &str) -> Vec<(String, String, i32)> {
        let mut v: Vec<_> = s
            .user_dict_entries(ds, name)
            .unwrap()
            .into_iter()
            .map(|e| (e.text, e.code, e.weight))
            .collect();
        v.sort();
        v
    }

    /// 加词按 (词, 码) 去重：已有的改成给的权重（可调低），没给权重就不动；新词没给权重时权重列留空。
    #[test]
    fn add_entry_upserts_by_text_and_code() {
        let s = store("add");
        s.import_user_dict("wb", "a", &[row("a", "工", 5)]).unwrap();
        assert!(
            s.add_user_dict_entry("wb", "a", "aa", "式", 0, Some(3))
                .unwrap()
        );
        assert!(
            s.add_user_dict_entry("wb", "a", "a", "工", 0, Some(9))
                .unwrap()
        );
        assert!(
            !s.add_user_dict_entry("wb", "a", "a", "工", 0, None)
                .unwrap()
        );
        assert!(
            !s.add_user_dict_entry("wb", "a", "a", "工", 0, Some(9))
                .unwrap()
        );
        assert!(
            s.add_user_dict_entry("wb", "a", "ab", "戈", 0, None)
                .unwrap()
        );
        assert!(
            s.add_user_dict_entry("wb", "a", "aa", "式", 0, Some(1))
                .unwrap()
        );
        assert_eq!(
            lib_rows(&s, "wb", "a"),
            [
                ("工".into(), "a".into(), 9),
                ("式".into(), "aa".into(), 1),
                ("戈".into(), "ab".into(), 0)
            ]
        );
        let p = lib_path(s.user_text_dir(), "wb", "a");
        assert!(std::fs::read_to_string(&p).unwrap().ends_with("ab\t戈\n"));
        for (code, text) in [
            ("", "x"),
            ("a", ""),
            ("a", "x\ty"),
            ("a\nb", "x"),
            ("a", "x\r"),
        ] {
            assert!(
                s.add_user_dict_entry("wb", "a", code, text, 0, Some(1))
                    .is_err(),
                "{code:?} {text:?}"
            );
        }
        assert!(
            s.add_user_dict_entry("wb", "无", "a", "x", 0, None)
                .is_err(),
            "分库不存在"
        );
    }

    /// 拼音分库：带空格的码按音节写。
    #[test]
    fn add_entry_keeps_syllables_in_pinyin_lib() {
        let s = store("add_py");
        s.create_user_dict("pinyin", "c").unwrap();
        s.add_user_dict_entry("pinyin", "c", "ni hao", "你好", 0, Some(2))
            .unwrap();
        let p = lib_path(s.user_text_dir(), "pinyin", "c");
        assert!(
            std::fs::read_to_string(&p)
                .unwrap()
                .contains("ni hao\t你好\t2\n")
        );
    }

    /// 清空：留文件头与说明行，删光词条；再清一次是 0。
    #[test]
    fn clear_keeps_header_and_banner() {
        let s = store("clear");
        s.import_user_dict(
            "pinyin",
            "c",
            &[row("ni hao", "你好", 1), row("a", "啊", 1)],
        )
        .unwrap();
        assert_eq!(s.clear_user_dict("pinyin", "c").unwrap(), 2);
        let text = std::fs::read_to_string(lib_path(s.user_text_dir(), "pinyin", "c")).unwrap();
        assert!(text.starts_with(banner::USER_DICT_LIB), "{text}");
        assert!(text.contains("code_spacing: syllable"), "{text}");
        assert!(lib_rows(&s, "pinyin", "c").is_empty());
        assert_eq!(s.clear_user_dict("pinyin", "c").unwrap(), 0);
        assert!(s.clear_user_dict("pinyin", "无").is_err());
    }

    /// 清空按解析器的注释区算：`# no comment` 之后 `#` 开头的是词条，一并删掉；返回数与删掉的一致。
    #[test]
    fn clear_treats_hash_lines_after_no_comment_as_entries() {
        let s = store("clear_nc");
        let p = lib_path(s.user_text_dir(), "wb", "c");
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(
            &p,
            "---\nname: c\ncolumns: [text, code, weight]\n...\n# 注释\n# no comment\n#井\tjing\t2\n你\tni\t1\n",
        )
        .unwrap();
        assert_eq!(s.clear_user_dict("wb", "c").unwrap(), 2);
        assert!(lib_rows(&s, "wb", "c").is_empty());
        assert_eq!(
            std::fs::read_to_string(&p).unwrap(),
            "---\nname: c\ncolumns: [text, code, weight]\n...\n# 注释\n# no comment\n"
        );
    }

    /// 没有权重列的分库：给了权重 ⇒ 报错、文件不动；不给照常加。
    #[test]
    fn add_entry_with_weight_needs_weight_column() {
        let s = store("add_now");
        let p = lib_path(s.user_text_dir(), "wb", "n");
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(&p, "---\nname: n\ncolumns: [text, code]\n...\n工\ta\n").unwrap();
        let before = std::fs::read(&p).unwrap();
        let e = s
            .add_user_dict_entry("wb", "n", "b", "了", 0, Some(5))
            .unwrap_err()
            .to_string();
        assert!(e.contains("没有权重列"), "{e}");
        assert!(
            s.add_user_dict_entry("wb", "n", "a", "工", 0, Some(5))
                .is_err()
        );
        assert_eq!(std::fs::read(&p).unwrap(), before);
        assert!(
            s.add_user_dict_entry("wb", "n", "b", "了", 0, None)
                .unwrap()
        );
    }

    /// 外部编辑中：加词、清空都挡住，文件不动。
    #[test]
    fn add_and_clear_rejected_while_editing() {
        let s = store("add_edit");
        s.import_user_dict("wb", "a", &[row("a", "工", 5)]).unwrap();
        let sess = s.begin_edit("wb/dicts/a.dict.yaml").unwrap();
        let before = std::fs::read(&sess.path).unwrap();
        let e = s
            .add_user_dict_entry("wb", "a", "b", "x", 0, None)
            .unwrap_err()
            .to_string();
        assert!(e.contains("正在外部编辑"), "{e}");
        assert!(
            s.clear_user_dict("wb", "a")
                .unwrap_err()
                .to_string()
                .contains("正在外部编辑")
        );
        assert_eq!(std::fs::read(&sess.path).unwrap(), before);
        s.end_edit("wb/dicts/a.dict.yaml").unwrap();
        assert_eq!(s.clear_user_dict("wb", "a").unwrap(), 1);
    }

    /// 分库的上一版在 `.backup/` 里：改名跟着走，删除一并删掉；数据文件旁边没有 `.bak`。
    #[test]
    fn lib_backup_follows_rename_and_delete() {
        let s = store("bakmv");
        let root = s.user_text_dir().to_path_buf();
        s.import_user_dict("wb", "a", &[row("a", "工", 1)]).unwrap();
        let p = lib_path(&root, "wb", "a");
        assert!(s.remove_user_dict_entry("wb", "a", "a", "工").unwrap());
        let bak = root.join(".backup/wb/dicts/a.dict.yaml");
        assert!(bak.is_file());
        assert!(!root.join("wb/dicts/a.dict.yaml.bak").exists());
        s.rename_user_dict("wb", "a", "b").unwrap();
        assert!(!bak.exists());
        let bak_b = root.join(".backup/wb/dicts/b.dict.yaml");
        assert!(bak_b.is_file());
        assert!(!p.exists());
        s.delete_user_dict("wb", "b").unwrap();
        assert!(!bak_b.exists());
    }

    #[test]
    fn create_writes_banner_and_header_and_rejects_dup() {
        let s = store("create");
        let p = s.create_user_dict("pinyin", "成语 大全").unwrap();
        let text = std::fs::read_to_string(&p).unwrap();
        assert!(text.starts_with(banner::USER_DICT_LIB), "{text}");
        assert!(text.contains("code_spacing: syllable"), "{text}");
        assert_eq!(read_label(&p), "成语 大全");
        assert!(s.create_user_dict("pinyin", "成语 大全").is_err(), "重名");
        assert!(s.create_user_dict("pinyin", "a/b").is_err(), "非法名");
        let p = s.create_user_dict("wb", "x").unwrap();
        assert!(!std::fs::read_to_string(p).unwrap().contains("code_spacing"));
    }

    #[test]
    fn switches_default_on_and_persist_per_schema() {
        let s = store("switch");
        let root = s.user_text_dir().to_path_buf();
        s.create_user_dict("pinyin", "a").unwrap();
        s.create_user_dict("pinyin", "b").unwrap();
        let on = |schema: &str| -> Vec<bool> {
            s.list_user_dicts("pinyin", schema)
                .unwrap()
                .into_iter()
                .map(|l| l.enabled)
                .collect()
        };
        assert_eq!(on("pinyin"), [true, true], "出厂全开");
        s.set_user_dict_enabled("pinyin", "shuangpin", "b", false)
            .unwrap();
        assert_eq!(on("pinyin"), [true, true], "开关按方案");
        assert_eq!(on("shuangpin"), [true, false]);
        assert_eq!(off_names(&root, "pinyin", "shuangpin"), ["b"]);
        let meta = std::fs::read_to_string(meta_path(&root, "pinyin")).unwrap();
        assert!(meta.starts_with(banner::USER_DICTS_META), "{meta}");
        s.set_user_dict_enabled("pinyin", "shuangpin", "b", true)
            .unwrap();
        assert!(off_names(&root, "pinyin", "shuangpin").is_empty());
    }

    #[test]
    fn corrupt_meta_reads_all_on_and_is_kept_aside_on_write() {
        let s = store("corrupt");
        let root = s.user_text_dir().to_path_buf();
        s.create_user_dict("wb", "a").unwrap();
        let mp = meta_path(&root, "wb");
        std::fs::write(&mp, "[schemas.wb\noff = [").unwrap();
        assert!(off_names(&root, "wb", "wb").is_empty(), "坏文件按全开");
        assert_eq!(issues::list_under(&mp)[0].kind, IssueKind::Isolated);
        s.set_user_dict_enabled("wb", "wb", "a", false).unwrap();
        assert_eq!(off_names(&root, "wb", "wb"), ["a"]);
        let kept = std::fs::read_dir(root.join(".backup/wb"))
            .unwrap()
            .flatten()
            .filter(|e| {
                e.file_name()
                    .to_string_lossy()
                    .starts_with("dicts.toml.corrupt-")
            })
            .count();
        assert_eq!(kept, 1, "坏文件留存");
        assert!(matches!(
            issues::list_under(&mp)[0].kind,
            IssueKind::Rebuilt { .. }
        ));
        issues::clear(&mp);
    }

    #[test]
    fn import_rename_delete_and_remove_entry() {
        let s = store("ops");
        let root = s.user_text_dir().to_path_buf();
        let n = s
            .import_user_dict(
                "pinyin",
                "成语",
                &[
                    row("hou ji bo fa", "厚积薄发", 5),
                    row("ni hao", "你好", 0),
                    row("ni hao", "你好", 3),
                ],
            )
            .unwrap();
        assert_eq!(n, 2, "同码同词只留一条");
        assert!(s.import_user_dict("pinyin", "成语", &[]).is_err(), "已存在");
        let e = s.user_dict_entries("pinyin", "成语").unwrap();
        assert_eq!(e.len(), 2);
        assert_eq!(e[0].code, "houjibofa");
        assert_ne!(e[0].boundary, 0, "音节边界随空格保留");
        assert_eq!(e[1].weight, 3, "取较高权重");

        s.set_user_dict_enabled("pinyin", "pinyin", "成语", false)
            .unwrap();
        s.rename_user_dict("pinyin", "成语", "四字").unwrap();
        assert!(!lib_path(&root, "pinyin", "成语").exists());
        assert_eq!(read_label(&lib_path(&root, "pinyin", "四字")), "四字");
        assert_eq!(off_names(&root, "pinyin", "pinyin"), ["四字"]);
        assert!(s.rename_user_dict("pinyin", "无", "x").is_err(), "不存在");
        s.create_user_dict("pinyin", "别的").unwrap();
        assert!(
            s.rename_user_dict("pinyin", "四字", "别的").is_err(),
            "重名"
        );

        assert!(
            s.remove_user_dict_entry("pinyin", "四字", "ni hao", "你好")
                .unwrap()
        );
        assert!(
            !s.remove_user_dict_entry("pinyin", "四字", "ni hao", "你好")
                .unwrap()
        );
        assert_eq!(s.user_dict_entries("pinyin", "四字").unwrap().len(), 1);

        s.delete_user_dict("pinyin", "四字").unwrap();
        assert!(!lib_path(&root, "pinyin", "四字").exists());
        assert!(off_names(&root, "pinyin", "pinyin").is_empty());
        assert!(s.delete_user_dict("pinyin", "四字").is_err());
    }

    #[test]
    fn user_file_without_banner_stays_without_banner_after_remove() {
        let s = store("ownfile");
        let root = s.user_text_dir().to_path_buf();
        let p = lib_path(&root, "wb", "mine");
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(&p, "---\nname: 我的\n...\n工\ta\n式\taa\t2\n").unwrap();
        let l = s.list_user_dicts("wb", "wb").unwrap();
        assert_eq!(
            (l[0].label.as_str(), l[0].entries, l[0].file.as_str()),
            ("我的", 2, "wb/dicts/mine.dict.yaml")
        );
        assert!(s.remove_user_dict_entry("wb", "mine", "a", "工").unwrap());
        assert_eq!(
            std::fs::read_to_string(&p).unwrap(),
            "---\nname: 我的\n...\n式\taa\t2\n"
        );
    }

    #[test]
    fn fingerprint_changes_with_files_and_switches() {
        let s = store("fp");
        let root = s.user_text_dir().to_path_buf();
        let a = fingerprint(&root, "wb");
        s.create_user_dict("wb", "x").unwrap();
        let b = fingerprint(&root, "wb");
        assert_ne!(a, b);
        assert_eq!(b, fingerprint(&root, "wb"), "没变就相同");
        s.set_user_dict_enabled("wb", "wb", "x", false).unwrap();
        assert_ne!(b, fingerprint(&root, "wb"));
    }
}
