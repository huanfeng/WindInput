//! 主题原始加载 + base 单链继承深合并
//!
//! 与 Go 版本 `wind_input/pkg/theme/theme.go` 对齐（v3 schema）。
//! 存储格式 TOML：用 `toml::Value` 作中间表示，base 提供全量、派生主题深合并覆盖。
//!
//! 归一化（扁平人写形态 → 内存嵌套形态）分两段跑：每层解析后先跑一次
//! [`normalize_theme_for_merge`]（不含 fill/shape，理由见其文档），整条链合并完再跑一次
//! 完整的 [`normalize_theme`]，然后类型化。

use crate::normalize::{normalize_theme, normalize_theme_for_merge};
use crate::schema::{Meta, Theme};
use std::path::{Path, PathBuf};
use toml::Value;

/// 主题文件名（每个主题目录下唯一）。
pub const THEME_FILE: &str = "theme.toml";

/// 在多个主题目录中定位 `<name>` 的主题目录（靠前目录优先；用户目录可覆盖内置）。
///
/// 命中后继续看后面的目录有没有同名，确证是覆盖时打一条日志（措辞同
/// `Config::log_user_override`，可按 `用户覆盖生效` 一次 grep 出全部覆盖）。主题定位的全部
/// 路径都经过这里，打点不会漏。
pub fn find_theme_dir(dirs: &[PathBuf], name: &str) -> Option<PathBuf> {
    // 名字来自 `base = "…"`（不可信主题文本）与 RPC 参数：只认普通 slug，别让 `..` /
    // 绝对路径把查找带出主题目录群。主题定位全都经过这里，一处挡住全部入口。
    if !is_plain_slug(name) {
        tracing::warn!("主题名不是普通 slug，忽略: {name:?}");
        return None;
    }
    let mut hit: Option<PathBuf> = None;
    for d in dirs {
        let p = d.join(name);
        if !p.join(THEME_FILE).exists() {
            continue;
        }
        match &hit {
            None => hit = Some(p),
            Some(w) => {
                tracing::info!("用户覆盖生效[theme]: {} → {}", name, w.display());
                break;
            }
        }
    }
    hit
}

/// 主题 base 链的目录列表（self 在前，base 在后）。
/// 用于资产（图片/SVG）字面 ref 解析：base 主题（如 _base）的 chevron 在其自身目录，派生主题继承后
/// 需到 base 目录查找该文件。
pub fn theme_chain_dirs(dirs: &[PathBuf], name: &str) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = Vec::new();
    let mut cur = name.to_string();
    for _ in 0..9 {
        let Some(d) = find_theme_dir(dirs, &cur) else {
            break;
        };
        if out.contains(&d) {
            break;
        }
        let base = std::fs::read_to_string(d.join(THEME_FILE))
            .ok()
            .and_then(|t| t.parse::<Value>().ok())
            .and_then(|v| {
                v.get("base")
                    .and_then(|b| b.as_str())
                    .map(|s| s.to_string())
            });
        out.push(d);
        match base {
            Some(b) if !b.is_empty() && b != cur => cur = b,
            _ => break,
        }
    }
    out
}

/// 读取主题自身 toml 的 meta（不做 base 合并；用于主题列表显示 name/order）。
pub fn read_meta(dirs: &[PathBuf], name: &str) -> Option<Meta> {
    let dir = find_theme_dir(dirs, name)?;
    let text = std::fs::read_to_string(dir.join(THEME_FILE)).ok()?;
    meta_from_text(&text)
}

/// 从主题 toml 文本解析其 meta（不读盘；用于导入时取主题名）。
pub fn meta_from_text(text: &str) -> Option<Meta> {
    let v: Value = text.parse().ok()?;
    v.get("meta")?.clone().try_into().ok()
}

/// 主题 id / `base` 引用是否普通 slug：非空，不是 `.` / `..`，不含 `/`、`\\`、`:`、`..`
/// 与控制字符（于是也不可能是绝对路径或盘符路径）；也不是 Windows 保留设备名（`CON`、
/// `NUL.txt` 之类），不以点或空格结尾（Windows 会悄悄去掉，两个 id 落到同一目录），
/// 不含 8.3 短名形态（`~` 后跟数字，`VERYLO~1` 可能指向另一个长名目录），不以
/// `.wtheme-tmp` / `.wtheme-old` 结尾（包导入拿它们作兄弟临时目录）。
/// 主题目录名即 id，合法 id 都满足。
pub fn is_plain_slug(s: &str) -> bool {
    let lower = s.to_ascii_lowercase();
    !s.is_empty()
        && s != "."
        && !s.contains("..")
        && !s.ends_with(['.', ' '])
        && !lower.ends_with(".wtheme-tmp")
        && !lower.ends_with(".wtheme-old")
        && !looks_like_8dot3(s)
        && !is_windows_reserved_name(s)
        && !s
            .chars()
            .any(|c| matches!(c, '/' | '\\' | ':') || c.is_control())
}

/// 是否 Windows 8.3 短名形态（`VERYLO~1`、`AB~12.TOM`）：主名 ≤ 8、扩展名 ≤ 3，
/// 全 ASCII，主名以 `~数字` 结尾且前面非空。只拦这种形态——它可能是另一个长名目录的别名；
/// `Theme~2 Dark` 这类长名不受影响，免得已有主题变成列不出、删不掉的孤儿。
fn looks_like_8dot3(s: &str) -> bool {
    let (stem, ext) = s.split_once('.').unwrap_or((s, ""));
    s.is_ascii()
        && stem.len() <= 8
        && ext.len() <= 3
        && !ext.contains('.')
        && stem.rsplit_once('~').is_some_and(|(head, digits)| {
            !head.is_empty() && !digits.is_empty() && digits.bytes().all(|b| b.is_ascii_digit())
        })
}

/// 从显示名派生主题 id（调用方没给 slug 时用）：路径分隔符、`:`、控制字符与 8.3 短名
/// 里的 `~` 换成 `-`，`..` 折成 `-`，去掉首尾空白与末尾的点、空格；仍不合法（保留名之类）
/// 加 `theme-` 前缀，再不行或为空就用 `theme`。本就合法的名字原样返回。
pub fn slug_from_name(name: &str) -> String {
    if is_plain_slug(name.trim()) {
        return name.trim().to_string();
    }
    let mut chars = name.trim().chars().peekable();
    let mut s = String::new();
    while let Some(c) = chars.next() {
        let bad = matches!(c, '/' | '\\' | ':')
            || c.is_control()
            || (c == '~' && chars.peek().is_some_and(char::is_ascii_digit));
        s.push(if bad { '-' } else { c });
    }
    while s.contains("..") {
        s = s.replace("..", "-");
    }
    let s = s.trim_end_matches(['.', ' ']);
    if s.is_empty() {
        return "theme".to_string();
    }
    if is_plain_slug(s) {
        return s.to_string();
    }
    let prefixed = format!("theme-{s}");
    if is_plain_slug(&prefixed) {
        prefixed
    } else {
        "theme".to_string()
    }
}

/// Windows 保留设备名（不分大小写，按首个 `.` 前的部分比较，带不带扩展名都算）：
/// `CON` `PRN` `AUX` `NUL` `CONIN$` `CONOUT$`、`COM1`–`COM9` / `LPT1`–`LPT9` 及上标
/// `COM¹²³` / `LPT¹²³`。
pub fn is_windows_reserved_name(s: &str) -> bool {
    let stem = s
        .split('.')
        .next()
        .unwrap_or(s)
        .trim_end()
        .to_ascii_uppercase();
    if matches!(
        stem.as_str(),
        "CON" | "PRN" | "AUX" | "NUL" | "CONIN$" | "CONOUT$"
    ) {
        return true;
    }
    let rest = stem
        .strip_prefix("COM")
        .or_else(|| stem.strip_prefix("LPT"));
    matches!(
        rest,
        Some("1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9" | "¹" | "²" | "³")
    )
}

/// 校验主题 toml 文本可解析为合法 Theme（导入前校验）。Err 含原因。
pub fn validate_text(text: &str) -> anyhow::Result<()> {
    let v: Value = text
        .parse::<Value>()
        .map_err(|e| anyhow::anyhow!("TOML 解析失败: {}", e))?;
    if let Some(b) = v.get("base").and_then(|b| b.as_str())
        && !b.is_empty()
        && !is_plain_slug(b)
    {
        anyhow::bail!("base 只能写主题 id（不能含路径）：{b:?}");
    }
    let n = normalize_theme(v);
    let _theme: Theme = n
        .try_into()
        .map_err(|e| anyhow::anyhow!("主题结构非法: {}", e))?;
    Ok(())
}

/// 目录下全部主题 id（含 `theme.toml` 的子目录名，含 `_` 开头的抽象 base），按名排序。
///
/// 出厂主题的逐个测试（渲染 golden、角色色为空、标准色契约）都从这里枚举，新加一个出厂
/// 主题不会因为谁忘了改一张硬编码清单而漏测。
pub fn list_theme_ids(dir: &Path) -> Vec<String> {
    let mut ids: Vec<String> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| e.path().join("theme.toml").is_file())
        .filter_map(|e| e.file_name().to_str().map(str::to_string))
        // 旧版在 Linux/mac 上可能建出尾随空格/点之类的目录：列出来也选不了（定位只认普通 slug）。
        .filter(|id| is_plain_slug(id))
        .collect();
    ids.sort();
    ids
}

/// 加载并 base 深合并主题，解析为类型化 `Theme`（未求值的原始 schema）。
/// 合并在 Value 层完成（逐层归一化 → 合并 → 完整归一化 → 类型化），未知字段忽略（前向兼容）。
pub fn load_typed(themes_dir: &Path, name: &str) -> anyhow::Result<Theme> {
    load_typed_dirs(&[themes_dir.to_path_buf()], name)
}

/// 多目录版：base 可在任一目录（如用户主题 `base = "_base"` 继承内置 _base）。
pub fn load_typed_dirs(dirs: &[PathBuf], name: &str) -> anyhow::Result<Theme> {
    type_merged(load_merged_dirs(dirs, name, 0)?, name)
}

/// 不走文件系统的版本：主题文本 + 它自己的 id（可空）+ 按 id 取 base 主题文本。给没有文件系统的
/// 调用方用（主题编辑器的 wasm 预览把内置主题编进产物）；取不到 base 时由 `base_text` 给出错误
/// （调用方最清楚该怎么说明）。合并、归一化与类型化同 [`load_typed_dirs`]：base 引用只认普通 slug
/// （[`is_plain_slug`]），`base` 写成自己的 id 视同没写。错误信息含 TOML 的行列，可原样给用户看。
pub fn load_typed_text(
    text: &str,
    name: &str,
    base_text: &dyn Fn(&str) -> anyhow::Result<String>,
) -> anyhow::Result<Theme> {
    let label = if name.is_empty() { "(text)" } else { name };
    type_merged(load_merged_text_at(text, name, 0, base_text)?, label)
}

fn load_merged_text_at(
    text: &str,
    name: &str,
    depth: usize,
    base_text: &dyn Fn(&str) -> anyhow::Result<String>,
) -> anyhow::Result<Value> {
    if depth > 8 {
        anyhow::bail!("theme base chain too deep (cycle?) at {}", name);
    }
    let value: Value = text.parse::<Value>().map_err(|e| {
        if name.is_empty() {
            anyhow::anyhow!("parse theme: {e}")
        } else {
            anyhow::anyhow!("parse theme {name}: {e}")
        }
    })?;
    merge_layer(value, name, depth, &mut |base, d| {
        if !is_plain_slug(base) {
            anyhow::bail!("base 只能写主题 id（不能含路径）：{base:?}");
        }
        load_merged_text_at(&base_text(base)?, base, d, base_text)
    })
}

/// 合并完的 Value → 类型化 `Theme`（完整归一化 + 竖排变体）。
fn type_merged(merged: Value, name: &str) -> anyhow::Result<Theme> {
    let normalized = normalize_theme(merged);
    let vertical = vertical_variant(&normalized);
    let mut theme: Theme = normalized
        .try_into()
        .map_err(|e| anyhow::anyhow!("type theme {}: {}", name, e))?;
    if let Some(v) = vertical {
        theme.vertical =
            Some(Box::new(v.try_into().map_err(|e| {
                anyhow::anyhow!("type theme {} (vertical): {}", name, e)
            })?));
    }
    Ok(theme)
}

/// 能写 `vertical` 的视图节点：候选窗自己的那一套。tooltip / status / toast / toolbar / menu
/// 是别的窗口，不随候选窗方向换样式；写在它们下面的 `vertical` 被忽略。
///
/// 主题编辑器 `merge.ts` 的 `VERTICAL_NODES` 是同一份清单，改一边必须改另一边。
pub const VERTICAL_NODES: [&str; 12] = [
    "window",
    "preedit_bar",
    "candidate_list",
    "item",
    "index",
    "text",
    "comment",
    "comment_above",
    "accent_bar",
    "footer_bar",
    "mode_label",
    "divider",
];

/// 竖排变体：把 `views.<节点>.vertical` 子表深合并进该节点，其余原样。没有任何节点写
/// `vertical` 返回 `None`（绝大多数主题，零开销）。
///
/// 只认 [`VERTICAL_NODES`] 下的 `vertical`，其中的状态补丁一并合并。合并语义同 base 链；
/// 翻页箭头同样遵守「本层写了字符就让位继承来的图」（见 [`drop_inherited_arrow_images`]）。
fn vertical_variant(normalized: &Value) -> Option<Value> {
    let views = table_at(normalized, &["views"])?;
    if !VERTICAL_NODES.iter().any(|k| {
        views
            .get(*k)
            .and_then(|n| n.get("vertical"))
            .is_some_and(Value::is_table)
    }) {
        return None;
    }
    let mut out = normalized.clone();
    let views = table_at_mut(&mut out, &["views"])?;
    for key in VERTICAL_NODES {
        let Some(node) = views.get_mut(key) else {
            continue;
        };
        let patch = node.as_table_mut().and_then(|t| t.remove("vertical"));
        if let Some(patch @ Value::Table(_)) = patch {
            let mut base = std::mem::replace(node, Value::Table(Default::default()));
            if key == "footer_bar" {
                drop_stale_arrow_images(&mut base, &patch);
            }
            *node = merge(base, patch);
        }
    }
    Some(out)
}

/// 读取 themes_dir/<name>/theme.toml 并按 base 链深合并（单目录；兼容旧调用）。
pub fn load_merged(themes_dir: &Path, name: &str, depth: usize) -> anyhow::Result<Value> {
    load_merged_dirs_at(&[themes_dir.to_path_buf()], name, depth)
}

/// 多目录 base 深合并（base 在下、派生在上）。防御循环继承（最多 8 层）。
///
/// 返回的 Value 已逐层过一遍 [`normalize_theme_for_merge`]：视图节点已收进 `views`、简写已
/// 展开，但 fill 与 shape 仍是人写形态（理由见其文档）。要完整嵌套形态，再跑一次
/// [`normalize_theme`]（幂等）。
pub fn load_merged_dirs(dirs: &[PathBuf], name: &str, depth: usize) -> anyhow::Result<Value> {
    load_merged_dirs_at(dirs, name, depth)
}

fn load_merged_dirs_at(dirs: &[PathBuf], name: &str, depth: usize) -> anyhow::Result<Value> {
    if depth > 8 {
        anyhow::bail!("theme base chain too deep (cycle?) at {}", name);
    }
    let dir = find_theme_dir(dirs, name)
        .ok_or_else(|| anyhow::anyhow!("theme '{}' not found in {:?}", name, dirs))?;
    let path = dir.join(THEME_FILE);
    let text = std::fs::read_to_string(&path)
        .map_err(|e| anyhow::anyhow!("read theme {}: {}", path.display(), e))?;
    let value: Value = text
        .parse::<Value>()
        .map_err(|e| anyhow::anyhow!("parse theme {}: {}", path.display(), e))?;
    merge_layer(value, name, depth, &mut |base, d| {
        load_merged_dirs_at(dirs, base, d)
    })
}

/// 一层主题（已解析未归一化）→ 归一化后叠到它的 base 链上。`load_base(base 名, 深度)` 取 base
/// 链合并结果；目录加载与文本加载只在这一步不同。
fn merge_layer(
    value: Value,
    name: &str,
    depth: usize,
    load_base: &mut dyn FnMut(&str, usize) -> anyhow::Result<Value>,
) -> anyhow::Result<Value> {
    // 归一化在合并之前逐层各做一次。先合并再归一化会静默坏两处：简写遇部分覆盖丢值
    // （base `padding = [6, 8]` + 派生 `padding = { left = 20 }` 只剩 left）；扁平 `[item]` 与
    // 嵌套 `[views.item]` 混用时整块丢表。提前归一化后两边形态一致，深合并才是逐字段的。
    let value = normalize_theme_for_merge(value);

    // base 链继承：先加载 base，再用本主题覆盖（merge）。
    if let Some(base_name) = value
        .get("base")
        .and_then(|b| b.as_str())
        .filter(|b| !b.is_empty() && *b != name)
    {
        let mut base = load_base(base_name, depth + 1)?;
        drop_inherited_arrow_images(&mut base, &value);
        crate::skin::replace_inherited_variants(&mut base, &value);
        return Ok(merge(base, value));
    }
    Ok(value)
}

/// 翻页箭头的互斥组：(字符键, 图键)。同一节点里两者只会用一个。
const ARROW_PAIRS: [(&str, &str); 2] = [("prev_char", "prev_image"), ("next_char", "next_image")];

/// 合并前：把「本主题显式写了字符、却只从 base 继承来的那张箭头图」丢掉。
///
/// 同层仍是图优先（字符作图解析不出时的兜底，见 `candidate_window.rs`）；只改跨层：派生
/// 主题写了 `prev_char = "$"`，要的就是 `$`，不该被 `_base` 继承来的 chevron 图挡死、逼作者
/// 再写 `prev_image = { ref = "" }` 抵消。判据是「本层写没写」而不是值。
///
/// 只看规范嵌套形态（`views.footer_bar`）：两边在 [`merge_layer`] 里已各自归一化过。
fn drop_inherited_arrow_images(base: &mut Value, over: &Value) {
    const PATH: [&str; 2] = ["views", "footer_bar"];
    let (Some(o), Some(b)) = (table_at(over, &PATH), table_at_mut(base, &PATH)) else {
        return;
    };
    drop_stale_arrow_images_in(b, o);
}

/// [`drop_inherited_arrow_images`] 作用在单个 footer_bar 节点上的版本（竖排覆盖用）。
fn drop_stale_arrow_images(base: &mut Value, over: &Value) {
    if let (Some(b), Some(o)) = (base.as_table_mut(), over.as_table()) {
        drop_stale_arrow_images_in(b, o);
    }
}

fn drop_stale_arrow_images_in(base: &mut toml::value::Table, over: &toml::value::Table) {
    for (c, i) in ARROW_PAIRS {
        if over.contains_key(c) && !over.contains_key(i) {
            base.remove(i);
        }
    }
}

/// 按键路径取表（任一段不是表则 None）。
fn table_at<'a>(v: &'a Value, path: &[&str]) -> Option<&'a toml::value::Table> {
    path.iter()
        .try_fold(v, |cur, k| cur.get(k))
        .and_then(Value::as_table)
}

/// [`table_at`] 的可变版。
fn table_at_mut<'a>(v: &'a mut Value, path: &[&str]) -> Option<&'a mut toml::value::Table> {
    path.iter()
        .try_fold(v, |cur, k| cur.get_mut(k))
        .and_then(Value::as_table_mut)
}

/// 深合并：over 覆盖 base。表递归合并；其余（标量/数组）由 over 覆盖。
pub fn merge(base: Value, over: Value) -> Value {
    match (base, over) {
        (Value::Table(mut b), Value::Table(o)) => {
            for (k, ov) in o {
                let merged = match b.remove(k.as_str()) {
                    Some(bv) => merge(bv, ov),
                    None => ov,
                };
                b.insert(k, merged);
            }
            Value::Table(b)
        }
        // 非表：over 优先
        (_, over) => over,
    }
}

#[cfg(test)]
#[path = "theme_tests.rs"]
mod tests;
