//! 设置端软键盘编辑器的纯逻辑层（`softkeyboard.*` RPC 的本体）：编辑视图 + 用户层稀疏写入。
//!
//! # ★ 写侧纪律（设计文档 §2.2）
//!
//! 只写**用户层**，且只写两样东西：面的 `name`、单键补丁 `keys`。
//! **永不写 `rows` / `rows_shift`**——画布只有一个写入者（作者，或手写整份文件的用户）。
//! 把展开后的内存表倒灌回画布，就是「整体替换 ⊕ 稀疏 diff」的数据丢失：出厂改了画布，
//! 用户这边那份倒灌进来的旧画布会把新内容整面盖掉。
//!
//! 由此：
//! - 改一个键只动这一个键的补丁；改成与下层（出厂 ⊕ 定制 ⊕ 用户手写画布）一致的值，补丁自动消失；
//! - 「恢复默认」= 删掉补丁，出厂后续对这个键的修正自动送达；
//! - 用户手写的注释、格式、别名键名以外的内容原样保留（`toml_edit` 原地改）。
//!
//! 下层（`lower`）由调用方给出：出厂 data 层铺底、定制层叠加后的表，**不含用户层**。

use serde::{Deserialize, Serialize};
use toml_edit::{DocumentMut, Item, Table, value};

use crate::layout::{all_slots, normalize_slot, parse_patch_key};
use crate::table::{HOLE, RawFile, SoftKeyboardTable};

/// 新建用户文件时写在开头的说明。只在文件原本为空时加，已有文件不动它的头部。
const USER_FILE_HEADER: &str = "\
# 软键盘用户覆盖（设置 › 输入 › 软键盘 维护，也可手动编辑）。
# 按面 id 与出厂文件合并：只写改过的键，其余继承出厂。格式见安装目录 data/system.softkeyboard.toml。
";

// ───────────────────────── 视图 ─────────────────────────

/// 编辑器里的一个面。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PageView {
    pub id: String,
    pub name: String,
    /// 去掉用户改名后的名字（出厂面「恢复默认」回到这里）；用户新建的面同 `name`。
    pub default_name: String,
    /// 用户新建的面（下层没有这个 id）。只有它能删除；出厂面只能「恢复默认」。
    pub user_created: bool,
    pub send_keys: bool,
    /// 用户层里有这一面的条目（出厂面 = 改过，可「恢复默认」）。
    pub has_user_entry: bool,
    /// 47 个键位，按键盘行列顺序。
    pub keys: Vec<KeyView>,
}

/// 编辑器里的一个键位。`None` = 空键位。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct KeyView {
    pub slot: &'static str,
    pub base: Option<String>,
    pub shift: Option<String>,
    /// 去掉用户补丁后的值（「恢复默认」会回到这里）。
    pub default_base: Option<String>,
    pub default_shift: Option<String>,
    /// 用户层对这一层有补丁。
    pub base_patched: bool,
    pub shift_patched: bool,
}

/// 编辑视图：下层 ⊕ 用户层，逐键标出默认值与是否被用户补丁覆盖。
///
/// 用户层语法错误返回 `Err`——编辑器不能在一份读不懂的文件上继续写（写回会丢内容）。
pub fn view(lower: &SoftKeyboardTable, user_text: &str) -> Result<Vec<PageView>, String> {
    let raw = parse_user(user_text)?;
    let effective = merged(lower, &raw);
    let defaults = merged(lower, &without_keys(&raw));
    let pages = effective
        .pages()
        .iter()
        .map(|p| {
            let user_entry = raw.pages.iter().find(|rp| rp.id == p.id);
            let patched = |slot: &str, shift: bool| {
                user_entry.is_some_and(|rp| {
                    rp.keys
                        .keys()
                        .any(|k| parse_patch_key(k) == Some((slot_static(slot), shift)))
                })
            };
            let def = defaults.page(&p.id);
            let keys = all_slots()
                .map(|slot| KeyView {
                    slot,
                    base: p.output(slot, false).map(str::to_string),
                    shift: p.output(slot, true).map(str::to_string),
                    default_base: def.and_then(|d| d.output(slot, false)).map(str::to_string),
                    default_shift: def.and_then(|d| d.output(slot, true)).map(str::to_string),
                    base_patched: patched(slot, false),
                    shift_patched: patched(slot, true),
                })
                .collect();
            PageView {
                id: p.id.clone(),
                name: p.name.clone(),
                default_name: lower
                    .page(&p.id)
                    .map_or_else(|| p.name.clone(), |l| l.name.clone()),
                user_created: lower.page(&p.id).is_none(),
                send_keys: p.send_keys,
                has_user_entry: user_entry.is_some(),
                keys,
            }
        })
        .collect();
    Ok(pages)
}

fn slot_static(slot: &str) -> &'static str {
    normalize_slot(slot).unwrap_or("")
}

fn parse_user(text: &str) -> Result<RawFile, String> {
    toml::from_str(text).map_err(|e| format!("用户软键盘文件解析失败：{e}"))
}

/// 下层叠上用户层。保留空面：编辑器要看得见刚新建、还没填键的面。
fn merged(lower: &SoftKeyboardTable, raw: &RawFile) -> SoftKeyboardTable {
    let mut t = lower.clone();
    t.merge_raw(raw, true);
    t
}

fn without_keys(raw: &RawFile) -> RawFile {
    let mut r = RawFile {
        pages: raw.pages.clone(),
    };
    for p in &mut r.pages {
        p.keys.clear();
    }
    r
}

// ───────────────────────── 写入 ─────────────────────────

/// 一次编辑操作。设置端在「应用设置」时把对话框里积攒的操作按序整批发来。
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(tag = "op", rename_all = "camelCase")]
pub enum Op {
    /// 设一个键。`value` 为 `None` 或空串 = 让这个键空着（按下吃掉、没有输出）。
    #[serde(rename_all = "camelCase")]
    SetKey {
        page: String,
        slot: String,
        shift: bool,
        value: Option<String>,
    },
    /// 删掉这个键的用户补丁，回到默认。
    #[serde(rename_all = "camelCase")]
    ResetKey {
        page: String,
        slot: String,
        shift: bool,
    },
    /// 新建一面。`copy_from` 给了就把那一面当前的全部键抄成补丁。
    ///
    /// 新建的面恒是**符号面**，不继承 `send_keys`：键盘面点击发送真实按键、画的字只是显示，
    /// 编辑器不让改它；用户复制它正是为了得到一张能改的面。
    #[serde(rename_all = "camelCase")]
    CreatePage {
        id: String,
        name: String,
        #[serde(default)]
        copy_from: Option<String>,
    },
    #[serde(rename_all = "camelCase")]
    RenamePage { page: String, name: String },
    /// 删除用户新建的面。出厂面会被拒绝（它们只能恢复默认）。
    #[serde(rename_all = "camelCase")]
    DeletePage { page: String },
    /// 出厂面恢复默认：删掉用户层里这一面的整个条目。用户新建的面会被拒绝。
    #[serde(rename_all = "camelCase")]
    ResetPage { page: String },
}

/// 把 `ops` 依次应用到用户层文本上，返回新文本。任何一步失败整批作废（返回 `Err`，不产出半成品）。
pub fn apply(lower: &SoftKeyboardTable, user_text: &str, ops: &[Op]) -> Result<String, String> {
    // 先确认读得懂：在一份解析不了的文件上原地改，写回去的东西谁也说不清。
    parse_user(user_text)?;
    let mut doc: DocumentMut = user_text
        .parse()
        .map_err(|e| format!("用户软键盘文件解析失败：{e}"))?;
    let was_blank = user_text.trim().is_empty();
    for op in ops {
        apply_one(lower, &mut doc, op)?;
    }
    let mut out = doc.to_string();
    if was_blank && !out.trim().is_empty() {
        out = format!("{USER_FILE_HEADER}\n{}", out.trim_start());
    }
    // 自检：写出去的必须仍是一份合法、且能被运行时读懂的文件。
    parse_user(&out).map_err(|e| format!("内部错误：生成的软键盘文件无法解析（{e}）"))?;
    Ok(out)
}

fn apply_one(lower: &SoftKeyboardTable, doc: &mut DocumentMut, op: &Op) -> Result<(), String> {
    match op {
        Op::SetKey {
            page,
            slot,
            shift,
            value: v,
        } => {
            let slot = canon_slot(slot)?;
            ensure_page_exists(lower, doc, page)?;
            let want = v.as_deref().filter(|s| !s.is_empty()).map(str::to_string);
            set_key(lower, doc, page, slot, *shift, KeyEdit::Set(want))
        }
        Op::ResetKey { page, slot, shift } => {
            let slot = canon_slot(slot)?;
            ensure_page_exists(lower, doc, page)?;
            set_key(lower, doc, page, slot, *shift, KeyEdit::Reset)
        }
        Op::CreatePage {
            id,
            name,
            copy_from,
        } => create_page(lower, doc, id, name, copy_from.as_deref()),
        Op::RenamePage { page, name } => {
            ensure_page_exists(lower, doc, page)?;
            let name = valid_name(name)?;
            let factory = lower.page(page).map(|p| p.name.clone());
            if factory.as_deref() == Some(name) {
                if let Some(t) = entry_mut(doc, page)? {
                    t.remove("name");
                }
                cleanup(lower, doc, page)?;
            } else {
                ensure_entry(doc, page)?.insert("name", value(name));
            }
            Ok(())
        }
        Op::DeletePage { page } => {
            if lower.page(page).is_some() {
                return Err(format!("「{page}」是预置的面，不能删除，只能恢复默认"));
            }
            if !remove_entry(doc, page)? {
                return Err(format!("面「{page}」不存在"));
            }
            Ok(())
        }
        Op::ResetPage { page } => {
            if lower.page(page).is_none() {
                return Err(format!("「{page}」不是预置的面，没有默认可恢复"));
            }
            remove_entry(doc, page)?;
            Ok(())
        }
    }
}

enum KeyEdit {
    Set(Option<String>),
    Reset,
}

fn set_key(
    lower: &SoftKeyboardTable,
    doc: &mut DocumentMut,
    page: &str,
    slot: &'static str,
    shift: bool,
    edit: KeyEdit,
) -> Result<(), String> {
    // 先删掉这一层的所有补丁（含别名写法：`[` 与 `lbracket` 是同一个键），再按需写回一条
    // 规范名。只删不写的话，留下的别名条目在 BTreeMap 序里可能排在后面、把新值盖回去。
    if let Some(t) = entry_mut(doc, page)? {
        remove_patches(t, slot, shift);
    }
    if let KeyEdit::Set(want) = edit {
        // 下层值 = 去掉这条补丁后的生效值。与之相同就不写——补丁只记差异。
        let default = current_table(lower, doc)?
            .page(page)
            .and_then(|p| p.output(slot, shift))
            .map(str::to_string);
        if want != default {
            let t = ensure_entry(doc, page)?;
            let keys = keys_table(t)?;
            keys.insert(&patch_key(slot, shift), value(encode(want.as_deref())));
        }
    }
    cleanup(lower, doc, page)
}

fn create_page(
    lower: &SoftKeyboardTable,
    doc: &mut DocumentMut,
    id: &str,
    name: &str,
    copy_from: Option<&str>,
) -> Result<(), String> {
    if id.is_empty()
        || !id
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '-')
    {
        return Err(format!("面 id「{id}」无效：只能用小写字母、数字、- 和 _"));
    }
    let name = valid_name(name)?;
    let current = current_table(lower, doc)?;
    if current.page(id).is_some() || entry_index(doc, id)?.is_some() {
        return Err(format!("面 id「{id}」已存在"));
    }
    let source = match copy_from {
        Some(src) => Some(
            current
                .page(src)
                .cloned()
                .ok_or_else(|| format!("要复制的面「{src}」不存在"))?,
        ),
        None => None,
    };
    let t = ensure_entry(doc, id)?;
    t.insert("name", value(name));
    if let Some(src) = source {
        let keys = keys_table(t)?;
        for (slot, shift, out) in src.entries() {
            keys.insert(&patch_key(slot, shift), value(encode(Some(out))));
        }
    }
    Ok(())
}

// ───────────────────────── 文档操作 ─────────────────────────

fn canon_slot(slot: &str) -> Result<&'static str, String> {
    normalize_slot(slot).ok_or_else(|| format!("「{slot}」不是软键盘可映射的键位"))
}

fn valid_name(name: &str) -> Result<&str, String> {
    let n = name.trim();
    if n.is_empty() {
        return Err("面的名称不能为空".into());
    }
    Ok(n)
}

fn ensure_page_exists(
    lower: &SoftKeyboardTable,
    doc: &DocumentMut,
    page: &str,
) -> Result<(), String> {
    if current_table(lower, doc)?.page(page).is_some() {
        Ok(())
    } else {
        Err(format!("面「{page}」不存在"))
    }
}

/// 文档当前的生效表（下层 ⊕ 正在编辑的用户层）。
fn current_table(
    lower: &SoftKeyboardTable,
    doc: &DocumentMut,
) -> Result<SoftKeyboardTable, String> {
    Ok(merged(lower, &parse_user(&doc.to_string())?))
}

fn patch_key(slot: &str, shift: bool) -> String {
    if shift {
        format!("shift+{slot}")
    } else {
        slot.to_string()
    }
}

/// 输出 → 补丁值。`None` 写占位符 `.`（空键位）；字面的 `.` 要转义成 `\.`。
fn encode(out: Option<&str>) -> String {
    match out {
        None => HOLE.to_string(),
        Some(HOLE) => "\\.".to_string(),
        Some(s) => s.to_string(),
    }
}

/// `[[pages]]` 数组。文件里没有就当作空（只读时）。
fn pages_array(doc: &DocumentMut) -> Result<Option<&toml_edit::ArrayOfTables>, String> {
    match doc.get("pages") {
        None => Ok(None),
        Some(Item::ArrayOfTables(a)) => Ok(Some(a)),
        Some(_) => Err(not_aot()),
    }
}

fn not_aot() -> String {
    "用户软键盘文件里的 pages 不是 [[pages]] 写法，设置界面无法安全改写，请手动编辑".into()
}

fn entry_index(doc: &DocumentMut, id: &str) -> Result<Option<usize>, String> {
    Ok(pages_array(doc)?.and_then(|a| {
        a.iter()
            .position(|t| t.get("id").and_then(Item::as_str) == Some(id))
    }))
}

fn entry_mut<'a>(doc: &'a mut DocumentMut, id: &str) -> Result<Option<&'a mut Table>, String> {
    let Some(i) = entry_index(doc, id)? else {
        return Ok(None);
    };
    match doc.get_mut("pages") {
        Some(Item::ArrayOfTables(a)) => Ok(a.get_mut(i)),
        _ => Err(not_aot()),
    }
}

fn ensure_entry<'a>(doc: &'a mut DocumentMut, id: &str) -> Result<&'a mut Table, String> {
    if entry_index(doc, id)?.is_none() {
        if doc.get("pages").is_none() {
            doc.insert(
                "pages",
                Item::ArrayOfTables(toml_edit::ArrayOfTables::new()),
            );
        }
        let Some(Item::ArrayOfTables(a)) = doc.get_mut("pages") else {
            return Err(not_aot());
        };
        let mut t = Table::new();
        t.insert("id", value(id));
        a.push(t);
    }
    entry_mut(doc, id)?.ok_or_else(|| "内部错误：新建的面条目找不到".to_string())
}

fn remove_entry(doc: &mut DocumentMut, id: &str) -> Result<bool, String> {
    let Some(i) = entry_index(doc, id)? else {
        return Ok(false);
    };
    if let Some(Item::ArrayOfTables(a)) = doc.get_mut("pages") {
        a.remove(i);
        if a.is_empty() {
            doc.remove("pages");
        }
    }
    Ok(true)
}

/// 条目的 `keys` 表（没有就新建一张 `[pages.keys]` 子表：一键一行，比内联表好读）。
fn keys_table(t: &mut Table) -> Result<&mut dyn toml_edit::TableLike, String> {
    if t.get("keys").is_none() {
        t.insert("keys", Item::Table(Table::new()));
    }
    t.get_mut("keys")
        .and_then(Item::as_table_like_mut)
        .ok_or_else(|| "用户软键盘文件里某一面的 keys 不是表，请手动修改".to_string())
}

fn remove_patches(t: &mut Table, slot: &str, shift: bool) {
    let Some(keys) = t.get_mut("keys").and_then(Item::as_table_like_mut) else {
        return;
    };
    let hits: Vec<String> = keys
        .iter()
        .map(|(k, _)| k.to_string())
        .filter(|k| parse_patch_key(k) == Some((slot_static(slot), shift)))
        .collect();
    for k in hits {
        keys.remove(&k);
    }
}

/// 收尾：空的 `keys` 表删掉；出厂面的条目只剩 `id` 时整条删掉（文件里不留无意义的空壳）。
fn cleanup(lower: &SoftKeyboardTable, doc: &mut DocumentMut, page: &str) -> Result<(), String> {
    let builtin = lower.page(page).is_some();
    let empty_shell = match entry_mut(doc, page)? {
        None => return Ok(()),
        Some(t) => {
            if t.get("keys")
                .and_then(Item::as_table_like)
                .is_some_and(|k| k.is_empty())
            {
                t.remove("keys");
            }
            builtin && t.iter().all(|(k, _)| k == "id")
        }
    };
    if empty_shell {
        remove_entry(doc, page)?;
    }
    Ok(())
}

#[cfg(test)]
#[path = "admin_tests.rs"]
mod tests;
