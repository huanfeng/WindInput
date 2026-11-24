//! 字体名的纯逻辑（渲染端 wind-ui 用；设置端 wind-setting 抄了一份 [`pick_localized_name`]，
//! 它那边的测试 `font_name_pick_matches_core` 钉住两份不漂移）。
//!
//! # 为什么需要它（论坛 t95 / 看板 A2-1）
//!
//! 字体命名有两套模型：GDI 每个 family 最多 4 个 face（常规/粗/斜/粗斜），七字重的
//! 「思源宋体」于是被拆成「思源宋体 SemiBold」「思源宋体 Medium」……若干个**独立 face name**；
//! DirectWrite 的 family 名**不含字重**，SemiBold 只是「思源宋体」下的一个 face。
//! 设置页早先用 GDI 枚举供给下拉、渲染端用 DirectWrite `FindFamilyName` 查找，
//! 带字重后缀的名字于是一律查不到、静默回落默认字体。
//!
//! 现在两端统一到 DirectWrite 模型：配置存 **family + 可选字重**（`ui.font.family` +
//! `ui.font.weight`）。本模块提供两端都要用、且与 COM 无关的那几步：
//! - [`split_weight_suffix`]：存量配置里的旧 GDI face name → (family, 字重) 的候选拆分；
//! - [`pick_localized_name`]：DirectWrite 本地化名表里挑显示/存储用的那一个。

/// 字重词 → OpenType `usWeightClass`。键是**归一化**后的形式（小写、去掉空格/连字符/下划线），
/// 故 `Semi Bold` / `semi-bold` / `SemiBold` 同义。
///
/// 取值与 DirectWrite `DWRITE_FONT_WEIGHT_*` 逐一对应（350 = SEMI_LIGHT，950 = EXTRA_BLACK）。
const WEIGHT_WORDS: &[(&str, i32)] = &[
    ("thin", 100),
    ("hairline", 100),
    ("extralight", 200),
    ("ultralight", 200),
    ("light", 300),
    ("semilight", 350),
    ("demilight", 350),
    ("regular", 400),
    ("normal", 400),
    ("book", 400),
    ("medium", 500),
    ("semibold", 600),
    ("demibold", 600),
    ("bold", 700),
    ("extrabold", 800),
    ("ultrabold", 800),
    ("black", 900),
    ("heavy", 900),
    ("extrablack", 950),
    ("ultrablack", 950),
    // 少数中文字体的本地化 face name 用中文字重词。
    ("极细", 200),
    ("纤细", 200),
    ("细体", 300),
    ("常规", 400),
    ("中等", 500),
    ("半粗", 600),
    ("粗体", 700),
    ("特粗", 800),
    ("超粗", 900),
];

fn weight_of_word(word: &str) -> Option<i32> {
    let norm: String = word
        .chars()
        .filter(|c| !matches!(c, ' ' | '-' | '_'))
        .flat_map(char::to_lowercase)
        .collect();
    WEIGHT_WORDS
        .iter()
        .find(|(w, _)| *w == norm)
        .map(|(_, v)| *v)
}

/// 把「family + 空格 + 字重词」形式的名字拆成 `(family, 字重)`；不是这种形式返回 `None`。
///
/// 只认**以空白分隔的结尾**一到两个词（两个词覆盖 `Semi Bold` / `Extra Light` 这类写法，
/// 先试两个词）。`family` 部分去掉首尾空白后不得为空——单独一个「Bold」不是字体名。
///
/// ⚠️ 这只是**候选**拆分：`Arial Black`、`Segoe UI Light` 这类名字本身就是 DirectWrite
/// 的合法 family 名。调用方必须**先**按原串查 family，查不到才试拆分，且拆出的 family
/// 也要再查一次存在性，否则会把用户写对的名字拆坏。
pub fn split_weight_suffix(name: &str) -> Option<(&str, i32)> {
    let name = name.trim();
    let words: Vec<(usize, &str)> = name
        .char_indices()
        .filter(|(i, c)| {
            !c.is_whitespace() && (*i == 0 || name[..*i].ends_with(char::is_whitespace))
        })
        .map(|(i, _)| (i, &name[i..]))
        .collect();
    // words[k] = (起始字节, 从该词起到结尾的子串)
    for take in [2usize, 1] {
        if words.len() <= take {
            continue;
        }
        let (start, tail) = words[words.len() - take];
        if let Some(w) = weight_of_word(tail) {
            let family = name[..start].trim_end();
            if !family.is_empty() {
                return Some((family, w));
            }
        }
    }
    None
}

/// 从 DirectWrite 的本地化名表 `[(locale, name)]` 里挑一个：`zh-cn` 优先，其次 `en-us`，
/// 都没有取第一条。locale 比较不分大小写（DirectWrite 返回的是 `zh-cn` / `zh-CN` 不一）。
///
/// 挑出来的名字既用于设置页显示，也**原样存进配置**：`FindFamilyName` 跨全部 locale 匹配，
/// 所以任何一个本地化名都能被渲染端查回来，挑中文只是为了让中文用户认得出。
pub fn pick_localized_name(names: &[(String, String)]) -> Option<String> {
    let by = |loc: &str| {
        names
            .iter()
            .find(|(l, n)| l.eq_ignore_ascii_case(loc) && !n.trim().is_empty())
            .map(|(_, n)| n.clone())
    };
    by("zh-cn").or_else(|| by("en-us")).or_else(|| {
        names
            .iter()
            .find(|(_, n)| !n.trim().is_empty())
            .map(|(_, n)| n.clone())
    })
}

/// 字体名比较：去首尾空白、ASCII 大小写不敏感（同 DirectWrite `FindFamilyName` 的口径）。
pub fn font_name_eq(a: &str, b: &str) -> bool {
    a.trim().eq_ignore_ascii_case(b.trim())
}

#[cfg(test)]
#[path = "font_name_tests.rs"]
mod tests;
