//! 主题原始加载 + base 单链继承深合并
//!
//! 与 Go 版本 `wind_input/pkg/theme/theme.go` 对齐（v3 schema）。
//! 存储格式 TOML：用 `toml::Value` 作中间表示，base 提供全量、派生主题深合并覆盖；
//! 合并后经 `normalize` 归一化（扁平人写形态 → 内存嵌套形态）再类型化。

use crate::normalize::normalize_theme;
use crate::schema::{Meta, Theme};
use std::path::{Path, PathBuf};
use toml::Value;

/// 主题文件名（每个主题目录下唯一）。
pub const THEME_FILE: &str = "theme.toml";

/// 在多个主题目录中定位 `<name>` 的主题目录（靠前目录优先；用户目录可覆盖内置）。
///
/// 命中后**不立即返回**，而是继续看后面的目录有没有同名——只为确证「这是覆盖而非
/// 用户独有主题」并打一条日志。主题定位的全部路径（`read_meta` / `load_typed_dirs` /
/// `theme_chain_dirs`）都经过这里，故打点放此处不会漏。措辞与 `Config::log_user_override`
/// 一致，便于按 `用户覆盖生效` 一次 grep 出全部生效的覆盖。
pub fn find_theme_dir(dirs: &[PathBuf], name: &str) -> Option<PathBuf> {
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
            .and_then(|t| toml::from_str::<Value>(&t).ok())
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
    let v: Value = toml::from_str(text).ok()?;
    v.get("meta")?.clone().try_into().ok()
}

/// 校验主题 toml 文本可解析为合法 Theme（导入前校验）。Err 含原因。
pub fn validate_text(text: &str) -> anyhow::Result<()> {
    let v: Value = toml::from_str(text).map_err(|e| anyhow::anyhow!("TOML 解析失败: {}", e))?;
    let n = normalize_theme(v);
    let _theme: Theme = n
        .try_into()
        .map_err(|e| anyhow::anyhow!("主题结构非法: {}", e))?;
    Ok(())
}

/// 加载并 base 深合并主题，解析为类型化 `Theme`（未求值的原始 schema）。
/// 合并在 Value 层完成（先合并后归一化再类型化），未知字段忽略（前向兼容）。
pub fn load_typed(themes_dir: &Path, name: &str) -> anyhow::Result<Theme> {
    load_typed_dirs(&[themes_dir.to_path_buf()], name)
}

/// 多目录版：base 可在任一目录（如用户主题 `base = "_base"` 继承内置 _base）。
pub fn load_typed_dirs(dirs: &[PathBuf], name: &str) -> anyhow::Result<Theme> {
    let merged = load_merged_dirs(dirs, name, 0)?;
    let normalized = normalize_theme(merged);
    let theme: Theme = normalized
        .try_into()
        .map_err(|e| anyhow::anyhow!("type theme {}: {}", name, e))?;
    Ok(theme)
}

/// 读取 themes_dir/<name>/theme.toml 并按 base 链深合并（单目录；兼容旧调用）。
pub fn load_merged(themes_dir: &Path, name: &str, depth: usize) -> anyhow::Result<Value> {
    load_merged_dirs_at(&[themes_dir.to_path_buf()], name, depth)
}

/// 多目录 base 深合并（base 在下、派生在上）。防御循环继承（最多 8 层）。
/// 返回**扁平人写形态**的合并 Value（未归一化；归一化在 `load_typed_dirs` 内）。
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
    let value: Value = toml::from_str(&text)
        .map_err(|e| anyhow::anyhow!("parse theme {}: {}", path.display(), e))?;

    // base 链继承：先加载 base（跨目录查找），再用本主题覆盖（merge）。
    if let Some(base_name) = value
        .get("base")
        .and_then(|b| b.as_str())
        .filter(|b| !b.is_empty() && *b != name)
    {
        let mut base = load_merged_dirs_at(dirs, base_name, depth + 1)?;
        drop_inherited_arrow_images(&mut base, &value);
        return Ok(merge(base, value));
    }
    Ok(value)
}

/// 翻页箭头的互斥组：(字符键, 图键)。同一节点里两者只会用一个。
const ARROW_PAIRS: [(&str, &str); 2] = [("prev_char", "prev_image"), ("next_char", "next_image")];

/// 合并前：把「本主题显式写了字符、却只从 base 继承来的那张箭头图」丢掉。
///
/// 同层优先级不变 —— 一个主题自己同时写了图和字符，仍是图优先（字符留作图 `ref` 解析
/// 不出时的兜底，见 `candidate_window.rs` 的三档）。变的只是**跨层**：
///
/// `_base` 给了 chevron SVG，而派生主题写 `[footer_bar] prev_char = "$"` 时，它要的显然
/// 是那个 `$`；让继承来的图把它挡死，作者就必须再补一行 `prev_image = { ref = "" }` 去
/// 抵消一个自己从没写过的默认值 —— 这个动作没有人猜得到（2026-09-20 的用户反馈正是如此，
/// 而 `_base` 那张图偏偏又是七个内置主题全都清掉、从未真正示人的一张）。
///
/// 判据是「本层写没写」而不是「值是什么」：显式写的压过继承来的，与 `ref = ""` 这种
/// 显式清空并不冲突（那是本层写的，照样生效）。
///
/// 扁平（`[footer_bar]`，主题文件的人写形态）与规范嵌套（`[views.footer_bar]`，
/// `normalize` 的产物形态）两种位置都看：合并跑在 normalize 之前，但两种形态都能进到
/// 这里，只认一种就会留下静默的盲区。
fn drop_inherited_arrow_images(base: &mut Value, over: &Value) {
    for path in [
        ["footer_bar"].as_slice(),
        ["views", "footer_bar"].as_slice(),
    ] {
        let Some(o) = table_at(over, path) else {
            continue;
        };
        let stale: Vec<&str> = ARROW_PAIRS
            .iter()
            .filter(|(c, i)| o.contains_key(*c) && !o.contains_key(*i))
            .map(|(_, i)| *i)
            .collect();
        if stale.is_empty() {
            continue;
        }
        let Some(b) = table_at_mut(base, path) else {
            continue;
        };
        for key in stale {
            b.remove(key);
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
mod tests {
    use super::*;

    #[test]
    fn meta_from_text_extracts_name() {
        let s = "[meta]\nname = \"测试主题\"\nauthor = \"me\"\n";
        let m = meta_from_text(s).expect("应解析出 meta");
        assert_eq!(m.name, "测试主题");
        assert_eq!(m.author, "me");
        // 无 meta → None
        assert!(meta_from_text("foo = 1\n").is_none());
    }

    #[test]
    fn validate_text_accepts_valid_rejects_garbage() {
        assert!(validate_text("[meta]\nname = \"ok\"\n").is_ok());
        // 非法 TOML
        assert!(validate_text("  : : :\n\t- bad").is_err());
    }

    #[test]
    fn load_typed_dirs_rejects_missing_base() {
        // 派生主题引用不存在的 base：load_merged_dirs_at 的 find_theme_dir 应报错，
        // 供导入链路（web_theme_import_text）据此判定依赖校验失败并回滚。
        let dir =
            std::env::temp_dir().join(format!("wind_theme_missing_base_{}", std::process::id()));
        let theme_dir = dir.join("derived");
        std::fs::create_dir_all(&theme_dir).unwrap();
        std::fs::write(
            theme_dir.join(THEME_FILE),
            "base = \"no-such-base\"\n[meta]\nname = \"derived\"\n",
        )
        .unwrap();

        let err = load_typed_dirs(std::slice::from_ref(&dir), "derived").unwrap_err();
        assert!(
            err.to_string().contains("no-such-base"),
            "错误信息应指出缺失的 base 主题名: {err}"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn merge_deep_overrides() {
        let base: Value = toml::from_str("[window]\npadding = 8\nradius = 4\n").unwrap();
        let over: Value = toml::from_str("[window]\nradius = 6\n").unwrap();
        let m = merge(base, over);
        let w = m.get("window").unwrap();
        assert_eq!(w.get("padding").unwrap().as_integer(), Some(8)); // base 保留
        assert_eq!(w.get("radius").unwrap().as_integer(), Some(6)); // over 覆盖
    }

    /// **inline table** 同样深合并——子主题只写其中一个键，base 的其余键必须留下。
    ///
    /// 上面那条测的是表节点（`[window]`）层。inline table 是另一种书写形态，主题里
    /// 大量用于 `prev_image = { ref, mode, tint, ... }`、`border = { width, color }`
    /// 这类聚合值。若这层退化成整表替换，子主题写 `{ tint = "…" }` 换个颜色就会顺手
    /// 抹掉 `ref`，箭头静默退化成文字 ‹ ›：主题照常加载、无报错、无日志，只有肉眼
    /// 能发现。故与表节点层分开各钉一次。
    #[test]
    fn merge_deep_overrides_inline_tables() {
        let base: Value =
            toml::from_str("[footer_bar]\nprev_image = { ref = \"a.svg\", mode = \"center\", tint = \"#111111\" }\n")
                .unwrap();
        let over: Value =
            toml::from_str("[footer_bar]\nprev_image = { tint = \"#222222\" }\n").unwrap();
        let img = merge(base, over);
        let img = img
            .get("footer_bar")
            .and_then(|f| f.get("prev_image"))
            .expect("prev_image");
        assert_eq!(img.get("ref").and_then(|v| v.as_str()), Some("a.svg"));
        assert_eq!(img.get("mode").and_then(|v| v.as_str()), Some("center"));
        assert_eq!(img.get("tint").and_then(|v| v.as_str()), Some("#222222"));
    }

    /// 合并 + 互斥规则的一次调用（测试里反复用到的两行）。
    fn merged(base: &str, over: &str) -> Value {
        let mut b: Value = toml::from_str(base).unwrap();
        let o: Value = toml::from_str(over).unwrap();
        drop_inherited_arrow_images(&mut b, &o);
        merge(b, o)
    }

    /// 取 `[footer_bar]` 下某键（不存在则 None）。
    fn footer<'a>(v: &'a Value, key: &str) -> Option<&'a Value> {
        v.get("footer_bar").and_then(|f| f.get(key))
    }

    /// 派生主题写了翻页字符 → 继承来的箭头图让位，字符才是作者要的东西。
    ///
    /// 这条不成立的话，任何 `base = "_base"` 的主题写 `prev_char` 都石沉大海，而
    /// 补救动作（`prev_image = { ref = "" }` 抵消一个自己没写过的默认值）无从猜起。
    #[test]
    fn explicit_char_drops_inherited_arrow_image() {
        let m = merged(
            "[footer_bar]\nprev_image = { ref = \"chevron.svg\" }\nnext_image = { ref = \"chevron2.svg\" }\n",
            "[footer_bar]\nprev_char = \"$\"\nnext_char = \")\"\n",
        );
        assert!(footer(&m, "prev_image").is_none(), "继承的上一页图应让位");
        assert!(footer(&m, "next_image").is_none(), "继承的下一页图应让位");
        assert_eq!(footer(&m, "prev_char").and_then(|v| v.as_str()), Some("$"));
    }

    /// 让位只按「本层写没写」判，与写的是什么值无关：
    /// 上一页写了字符 → 上一页的图让位；下一页没写 → 下一页的图原样继承。
    /// 两侧互不牵连，否则「只想改一边」的主题会莫名丢掉另一边的图。
    #[test]
    fn inherited_image_survives_on_the_side_without_char() {
        let m = merged(
            "[footer_bar]\nprev_image = { ref = \"a.svg\" }\nnext_image = { ref = \"b.svg\" }\n",
            "[footer_bar]\nprev_char = \"$\"\n",
        );
        assert!(footer(&m, "prev_image").is_none());
        assert_eq!(
            footer(&m, "next_image")
                .and_then(|i| i.get("ref"))
                .and_then(|v| v.as_str()),
            Some("b.svg"),
            "没写 next_char 的那侧不受影响"
        );
    }

    /// 同层仍是图优先：一个主题自己把图和字符都写上，图留着
    /// （字符退为图 `ref` 解析不出时的兜底）。让位只针对**继承来的**图。
    #[test]
    fn same_layer_image_and_char_both_kept() {
        let m = merged(
            "[footer_bar]\nfont_size = -4\n",
            "[footer_bar]\nprev_char = \"$\"\nprev_image = { ref = \"own.svg\" }\n",
        );
        assert_eq!(
            footer(&m, "prev_image")
                .and_then(|i| i.get("ref"))
                .and_then(|v| v.as_str()),
            Some("own.svg"),
            "本层自己写的图不该被本层自己写的字符挤掉"
        );
        assert_eq!(footer(&m, "prev_char").and_then(|v| v.as_str()), Some("$"));
    }

    /// `ref = ""`（_qingfeng / msime 清掉继承图的写法）仍是**本层写的图**，
    /// 照常保留 —— 它一路传到渲染层才解析成空、回退字符档，与本规则不冲突。
    #[test]
    fn explicit_empty_ref_is_still_an_own_image() {
        let m = merged(
            "[footer_bar]\nprev_image = { ref = \"chevron.svg\" }\n",
            "[footer_bar]\nprev_char = \"$\"\nprev_image = { ref = \"\" }\n",
        );
        assert_eq!(
            footer(&m, "prev_image")
                .and_then(|i| i.get("ref"))
                .and_then(|v| v.as_str()),
            Some(""),
            "显式清空是本层的意思, 该原样留着"
        );
    }

    /// 规范嵌套形态（`[views.footer_bar]`）同样受规则约束 —— 合并虽跑在 normalize
    /// 之前, 但主题文件本就可以直接写 views 表, 只认扁平形态会留下静默盲区。
    #[test]
    fn rule_applies_to_nested_views_form() {
        let m = merged(
            "[views.footer_bar]\nprev_image = { ref = \"chevron.svg\" }\n",
            "[views.footer_bar]\nprev_char = \"$\"\n",
        );
        let f = m
            .get("views")
            .and_then(|v| v.get("footer_bar"))
            .expect("views.footer_bar");
        assert!(f.get("prev_image").is_none(), "嵌套形态也应让位");
        assert_eq!(f.get("prev_char").and_then(|v| v.as_str()), Some("$"));
    }
}
