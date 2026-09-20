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
/// 合并在 Value 层完成（逐层归一化 → 合并 → 完整归一化 → 类型化），未知字段忽略（前向兼容）。
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
///
/// 返回的 Value **已逐层过一遍 [`normalize_theme_for_merge`]**：视图节点都已收进 `views`、
/// edges/radius 这类简写已展开，但 fill（background/icon/hole）与 shape 仍是人写形态 ——
/// 那两样要等合并完才展开（理由见 `normalize_theme_for_merge` 的文档）。
/// 跨 crate 的调用方若要完整嵌套形态，自己再跑一次 [`normalize_theme`]（幂等）。
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
    // **归一化在合并之前，逐层各做一次**（`normalize_theme` 对已归一化的输入幂等）。
    //
    // 顺序反过来（先合并整条链、最后归一化一次）会坏两处，两处都静默：
    //
    // 1. 简写遇上部分覆盖丢值：base 写 `padding = [6, 8]`、派生写 `padding = { left = 20 }`,
    //    合并时是「非表 vs 表」→ 整个取派生那张表 → 归一化后只剩 left, 上右下没了。
    //    而 `_base` 里简写用得到处都是, 这条撞上的概率远比下面那条高。
    // 2. 两种书写形态混用时整块丢表：扁平 `[item]` 与规范嵌套 `[views.item]` 在合并阶段是
    //    两个互不相干的键, 谁也盖不住谁, 最后归一化时才被 views 表的整块替换吃掉一边 ——
    //    而那时已经分不清哪半来自 base、哪半来自派生了。
    //
    // 归一化提前之后, 进 merge 的两边形态一致、简写都已展开, 深合并才是逐字段的。
    let value = normalize_theme_for_merge(value);

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
/// 只看规范嵌套形态（`views.footer_bar`）：本函数跑在 merge 之前，而那时两边都已各自
/// 归一化过（见上方 `load_merged_dirs_at` 里提前调用 `normalize_theme` 的理由），扁平
/// 形态的 `[footer_bar]` 早已被收进 `views` 了。
fn drop_inherited_arrow_images(base: &mut Value, over: &Value) {
    const PATH: [&str; 2] = ["views", "footer_bar"];
    let Some(o) = table_at(over, &PATH) else {
        return;
    };
    let stale: Vec<&str> = ARROW_PAIRS
        .iter()
        .filter(|(c, i)| o.contains_key(*c) && !o.contains_key(*i))
        .map(|(_, i)| *i)
        .collect();
    if stale.is_empty() {
        return;
    }
    let Some(b) = table_at_mut(base, &PATH) else {
        return;
    };
    for key in stale {
        b.remove(key);
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
    use crate::schema::Dim;

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

    /// 走一遍真实调用序列的中段：两层各自归一化 → 互斥规则 → 深合并。
    ///
    /// 归一化不能省：`load_merged_dirs_at` 就是这么排的，而 `drop_inherited_arrow_images`
    /// 只认归一化后的 `views.footer_bar`。喂扁平形态给它等于测一条不存在的路径。
    fn merged(base: &str, over: &str) -> Value {
        let mut b = normalize_theme(toml::from_str(base).unwrap());
        let o = normalize_theme(toml::from_str(over).unwrap());
        drop_inherited_arrow_images(&mut b, &o);
        merge(b, o)
    }

    /// 取归一化后 `views.footer_bar` 下某键（不存在则 None）。
    fn footer<'a>(v: &'a Value, key: &str) -> Option<&'a Value> {
        v.get("views")
            .and_then(|x| x.get("footer_bar"))
            .and_then(|f| f.get(key))
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
            "[footer_bar]\nprev_image = { ref = \"inherited.svg\", mode = \"center\" }\n",
            "[footer_bar]\nprev_char = \"$\"\nprev_image = { ref = \"own.svg\" }\n",
        );
        let img = footer(&m, "prev_image").expect("prev_image");
        assert_eq!(
            img.get("ref").and_then(|v| v.as_str()),
            Some("own.svg"),
            "本层自己写的图不该被本层自己写的字符挤掉"
        );
        // mode 是判别点：本层没写它, 只有「没让位、照常深合并」才留得下。
        // 只断言 ref 的话, 让位与否都得到 own.svg, 这条测试就永远不会红。
        assert_eq!(
            img.get("mode").and_then(|v| v.as_str()),
            Some("center"),
            "同层写了图 → 不该触发让位, 继承的图应照常深合并"
        );
        assert_eq!(footer(&m, "prev_char").and_then(|v| v.as_str()), Some("$"));
    }

    /// `ref = ""`（_qingfeng / msime 清掉继承图的写法）仍是**本层写的图**，
    /// 照常保留 —— 它一路传到渲染层才解析成空、回退字符档，与本规则不冲突。
    #[test]
    fn explicit_empty_ref_is_still_an_own_image() {
        let m = merged(
            "[footer_bar]\nprev_image = { ref = \"chevron.svg\", mode = \"center\" }\n",
            "[footer_bar]\nprev_char = \"$\"\nprev_image = { ref = \"\" }\n",
        );
        let img = footer(&m, "prev_image").expect("prev_image");
        assert_eq!(
            img.get("ref").and_then(|v| v.as_str()),
            Some(""),
            "显式清空是本层的意思, 该原样留着"
        );
        // 同上：ref 两条分支都得空串, mode 才是判别点。
        assert_eq!(
            img.get("mode").and_then(|v| v.as_str()),
            Some("center"),
            "本层写了图(哪怕是空 ref) → 不触发让位"
        );
    }

    /// 在临时目录里摆一条 `themebase ← derived` 的主题链，走真实加载链取回类型化结果。
    ///
    /// `tag` 只为让并发跑的用例各用各的目录。
    fn load_chain(tag: &str, base_toml: &str, derived_toml: &str) -> crate::schema::Theme {
        let dir = std::env::temp_dir().join(format!("wind_theme_{}_{}", tag, std::process::id()));
        // 断言失败时下面的 remove 不会执行 —— 开头先清一次，免得上一轮的残留影响这一轮。
        let _ = std::fs::remove_dir_all(&dir);
        let base_dir = dir.join("themebase");
        let derived_dir = dir.join("derived");
        std::fs::create_dir_all(&base_dir).unwrap();
        std::fs::create_dir_all(&derived_dir).unwrap();
        std::fs::write(base_dir.join(THEME_FILE), base_toml).unwrap();
        std::fs::write(derived_dir.join(THEME_FILE), derived_toml).unwrap();
        let t = load_typed_dirs(std::slice::from_ref(&dir), "derived").expect("load derived");
        let _ = std::fs::remove_dir_all(&dir);
        t
    }

    /// base 写简写、派生只覆盖其中一边时，其余三边必须还在。
    ///
    /// 归一化若排在整条链合并之后，这里是「非表 vs 表」的覆盖：`padding = [6, 8]` 整个被
    /// `{ left = 20 }` 顶掉，上右下凭空消失。`_base` 里简写用得到处都是（`padding = [6, 8]`、
    /// `margin = { left = 8 }`…），派生主题只想挪一边内边距是再常见不过的写法，两者一撞
    /// 就丢值，且主题照常加载、无报错无日志。
    #[test]
    fn shorthand_in_base_survives_partial_override() {
        let t = load_chain(
            "shorthand",
            "[meta]\nname = \"themebase\"\n[window]\npadding = [6, 8]\n",
            "base = \"themebase\"\n[meta]\nname = \"derived\"\n[window]\npadding = { left = 20 }\n",
        );
        let pad = &t.views.as_ref().expect("views").window.padding;
        let dp = |d: Option<Dim>| d.map(|d| d.resolve(1.0, 0.0));
        assert_eq!(dp(pad.left), Some(20.0), "派生覆盖的那边");
        assert_eq!(dp(pad.top), Some(6.0), "base 简写的上边不该丢");
        assert_eq!(dp(pad.right), Some(8.0), "右边不该丢");
        assert_eq!(dp(pad.bottom), Some(6.0), "下边不该丢");
    }

    /// 派生主题写一个纯色背景，就该把 base 那层的背景图盖掉 —— 整体重置，不是只换底色。
    ///
    /// 绘制顺序是底色 → 渐变 → 边框 → 背景图（`wind-ui/src/view.rs`），继承来的图若留着，
    /// 正好盖在派生刚写的底色上，表现是「派生的背景覆盖不生效」，而作者要抵消的是一个
    /// 自己从没写过的值。`background = "#0f0"` 这种标量写法在合并时是「标量 vs 表」，
    /// 靠的就是整体替换语义 —— 归一化若在合并前把它展成 `{ color = … }`，这条就没了。
    #[test]
    fn scalar_background_in_derived_resets_inherited_image() {
        let t = load_chain(
            "bgreset",
            "[meta]\nname = \"themebase\"\n[window]\nbackground = { color = \"#FF0000\", image = { ref = \"p.png\" } }\n",
            "base = \"themebase\"\n[meta]\nname = \"derived\"\n[window]\nbackground = \"#00FF00\"\n",
        );
        let bg = &t.views.as_ref().expect("views").window.background;
        assert!(
            bg.image.is_none(),
            "派生写了纯色 → 继承来的背景图应整体让位, 实得 {:?}",
            bg.image
        );
    }

    /// 派生主题把 slice_repeat 写错时，要退回拉伸，而不是静默继承 base 的 repeat。
    ///
    /// `expand_axes` 的注释承诺「认不出的形态一律丢弃 ⇒ 两轴拉伸」。它从前产出的是**空表**,
    /// 单层看没问题, 一进 base 深合并就什么也盖不住 —— 于是承诺反过来了: 作者写错一个值,
    /// 继承的 repeat 照旧生效, 而他以为自己把它关掉了。prev_image/layers 这类字段的归一化
    /// 排在合并之前（fill 里的那份已随背景一起延后），所以这条只在这几个字段上现形。
    #[test]
    fn bad_slice_repeat_in_derived_falls_back_to_stretch() {
        let t = load_chain(
            "sliceaxes",
            "[meta]\nname = \"themebase\"\n[footer_bar]\n\
             prev_image = { ref = \"a.svg\", mode = \"nine_slice\", slice_repeat = \"repeat\" }\n",
            "base = \"themebase\"\n[meta]\nname = \"derived\"\n[footer_bar]\n\
             prev_image = { ref = \"a.svg\", mode = \"nine_slice\", slice_repeat = 42 }\n",
        );
        let im = t
            .views
            .as_ref()
            .expect("views")
            .footer_bar
            .prev_image
            .as_ref()
            .expect("prev_image");
        assert_ne!(
            im.slice_repeat.x.as_deref(),
            Some("repeat"),
            "写错的值该退回拉伸, 不该继承 base 的 repeat"
        );
        assert_ne!(im.slice_repeat.y.as_deref(), Some("repeat"));
    }

    /// `[views.toolbar.button.mode.chinese]` 里的简写同样要展开。
    ///
    /// 归一化从前只把**扁平**的 `chinese`/`english` 搬进 `mode`，对已存在的 `mode` 表既不
    /// 递归也不合并（直接整块替换）。于是这种写法里的 `padding = 5` 走到 serde 是
    /// `invalid type: integer 5`，**整份主题加载失败**。
    #[test]
    fn shorthand_inside_nested_toolbar_mode_is_expanded() {
        let t = load_chain(
            "toolbarmode",
            "[meta]\nname = \"themebase\"\n",
            "base = \"themebase\"\n[meta]\nname = \"derived\"\n\
             [views.toolbar.button.mode.chinese]\npadding = 5\nradius = 2\n",
        );
        let cn = t
            .views
            .as_ref()
            .expect("views")
            .toolbar
            .as_ref()
            .expect("toolbar")
            .button
            .mode
            .as_ref()
            .map(|m| &m.chinese)
            .expect("mode.chinese");
        assert_eq!(
            cn.padding.top.map(|d| d.resolve(1.0, 0.0)),
            Some(5.0),
            "标量简写应已展开成四边"
        );
        assert_eq!(
            cn.border.radius.map(|d| d.resolve(1.0, 0.0)),
            Some(2.0),
            "radius → border.radius 的搬迁在这一层同样要做"
        );
    }

    /// 两层用不同书写形态（base 规范嵌套 / 派生扁平）时，两边的节点都要留下。
    ///
    /// 归一化排在合并之后的话，这两种形态在合并阶段是互不相干的两个键，最后归一化时
    /// `views` 被整块替换，base 那份连同里面别的节点一起蒸发 —— 箭头看着还对（派生自己
    /// 写了），丢的是 `views.item` 这类没人会去核对的东西。
    #[test]
    fn mixed_writing_forms_keep_both_sides() {
        let t = load_chain(
            "mixedform",
            "[meta]\nname = \"themebase\"\n[views.item]\npadding = 4\n[views.window]\npadding = 6\n",
            "base = \"themebase\"\n[meta]\nname = \"derived\"\n[window]\npadding = 12\n",
        );
        let v = t.views.as_ref().expect("views");
        assert_eq!(
            v.window.padding.top.map(|d| d.resolve(1.0, 0.0)),
            Some(12.0),
            "派生（扁平）覆盖 base（嵌套）的同名节点"
        );
        assert_eq!(
            v.item.padding.top.map(|d| d.resolve(1.0, 0.0)),
            Some(4.0),
            "base 那份 views 里没被派生碰过的节点必须留下 —— 整块替换正是从这里丢东西的"
        );
    }

    /// 规范嵌套形态里写简写不该炸：`normalize_node` 从前不作用于已存在的 `views` 表，
    /// 于是 `[views.window] padding = 6` 走到 serde 那里是 `invalid type: integer 6`,
    /// **整份主题加载失败**（不是这一项失效，是全盘皆输）。
    #[test]
    fn shorthand_inside_nested_views_is_expanded() {
        let t = load_chain(
            "nestedshorthand",
            "[meta]\nname = \"themebase\"\n",
            "base = \"themebase\"\n[meta]\nname = \"derived\"\n[views.window]\npadding = 6\nradius = 3\n",
        );
        let w = &t.views.as_ref().expect("views").window;
        let dp = |d: Option<Dim>| d.map(|d| d.resolve(1.0, 0.0));
        assert_eq!(dp(w.padding.top), Some(6.0), "标量简写应已展开成四边");
        assert_eq!(dp(w.padding.left), Some(6.0));
        assert_eq!(
            dp(w.border.radius),
            Some(3.0),
            "radius → border.radius 的搬迁在嵌套形态里同样要做"
        );
    }

    /// 走**真实加载链**（写盘 → load_typed_dirs → 合并 → normalize → 类型化）验一次让位。
    ///
    /// 上面那几条都直接调 `drop_inherited_arrow_images`，于是把 `load_merged_dirs_at` 里那行
    /// 调用删掉，它们一条都不会红 —— 而「字段/规则存在但没接到链上」正是这两笔修的 bug 本体
    /// （渲染层不读 prev_char），同一个坑不能在测试侧再踩一次。这条从磁盘上的两份 toml 出发，
    /// 挂得住调用点、normalize 与类型化整条链。
    #[test]
    fn inherited_image_yields_through_the_real_load_chain() {
        let dir =
            std::env::temp_dir().join(format!("wind_theme_arrow_chain_{}", std::process::id()));
        let base_dir = dir.join("themebase");
        let derived_dir = dir.join("derived");
        std::fs::create_dir_all(&base_dir).unwrap();
        std::fs::create_dir_all(&derived_dir).unwrap();
        std::fs::write(
            base_dir.join(THEME_FILE),
            "[meta]\nname = \"themebase\"\n[footer_bar]\n\
             prev_image = { ref = \"chevron_prev.svg\" }\n\
             next_image = { ref = \"chevron_next.svg\" }\n",
        )
        .unwrap();
        std::fs::write(
            derived_dir.join(THEME_FILE),
            "base = \"themebase\"\n[meta]\nname = \"derived\"\n[footer_bar]\nprev_char = \"$\"\n",
        )
        .unwrap();

        let t = load_typed_dirs(std::slice::from_ref(&dir), "derived").expect("load derived");
        let footer = &t.views.as_ref().expect("views").footer_bar;
        assert_eq!(
            footer.prev_char.as_deref(),
            Some("$"),
            "本层写的字符要活到类型化之后"
        );
        assert!(
            footer.prev_image.is_none(),
            "写了 prev_char 的那侧, 继承来的图应在合并层就让位"
        );
        assert!(
            footer.next_image.is_some(),
            "没写 next_char 的那侧, 继承来的图原样保留"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 规范嵌套形态（`[views.footer_bar]`）同样受规则约束 —— 合并虽跑在 normalize
    /// 之前, 但主题文件本就可以直接写 views 表, 只认扁平形态会留下静默盲区。
    #[test]
    fn rule_applies_to_nested_views_form() {
        let m = merged(
            "[views.footer_bar]\nprev_image = { ref = \"chevron.svg\" }\n",
            "[views.footer_bar]\nprev_char = \"$\"\n",
        );
        assert!(footer(&m, "prev_image").is_none(), "嵌套形态也应让位");
        assert_eq!(footer(&m, "prev_char").and_then(|v| v.as_str()), Some("$"));
    }
}
