//! 全拼击键的 ü 拼写归一化（`xv` → `xu`、`nue` → `nve`）。
//!
//! 拼音正字法里 j/q/x/y 后的 ü 写作 `u`，n/l 后保留两点（键盘用 `v`），所以
//! [`syllable::STANDARD_SYLLABLES`](super::syllable::STANDARD_SYLLABLES) 只收 `lv`/`nve` 一类。
//! 用户按习惯打 `xv` 或 `nue`/`lue` 会空码；本模块是双拼侧
//! [`shuangpin::normalize_pinyin`](super::shuangpin) 在全拼域的对应物。
//!
//! ## 两条规则的安全性不对称
//!
//! - `v` → `u`（前一字符 ∈ jqxy）：无损。`v` 在音节表里只跟在 l/n 后，且不能起音节，
//!   jqxy 后的 `v` 怎么切都是死路。
//! - `ue` → `ve`（前一字符 ∈ nl）：有损的取舍。`nue` 本能切成 `nu|e`，但那一族真实输入里
//!   不存在，而 nüe/lüe 是常用字（搜狗/微软同样判给「虐」）。逃生口是手动分隔符 `nu'e`。
//!
//! ## 不变量：等长 1:1 替换
//!
//! 两条规则都只换单个字节、不增删，raw/flat 两域的位置映射（`interp::SylSpan`、
//! `build_raw_preedit`、`consumed_length`）因此不用改。要加增删长度的规则（如 `ng` → `en`）
//! 须先处理这条不变量，不能顺手塞进本函数。

use std::borrow::Cow;

/// 判断字节是否为 j/q/x/y——这四个声母后的 ü 按正字法写作 `u`。
#[inline]
fn is_jqxy(b: u8) -> bool {
    matches!(b, b'j' | b'q' | b'x' | b'y')
}

/// 判断字节是否为 n/l——这两个声母后的 ü 必须保留两点，键盘上写作 `v`。
#[inline]
fn is_nl(b: u8) -> bool {
    matches!(b, b'n' | b'l')
}

/// 把全拼击键串里的 ü 变体拼写归一到音节表使用的正字法形态。
///
/// - `xv` → `xu`、`jvn` → `jun`、`qve` → `que`（jqxy 后的 `v` 即 `u`）
/// - `nue` → `nve`、`lue` → `lve`（n/l 后的 `ue` 即 `üe`）
/// - `nv`/`lv`/`lve` 原样保留（它们已经是音节表的形态）
///
/// 无需替换时返回 [`Cow::Borrowed`]，不分配——本函数在按键热路径上。
///
/// 只接受全拼域的串。双拼击键里 `v` 是布局键（小鹤 `v` = zh 声母），
/// 拿到这里会被替换成 `u`、毁掉整个双拼输入。调用方必须自行确认域。
pub fn normalize_u_umlaut(input: &str) -> Cow<'_, str> {
    let bytes = input.as_bytes();
    // 快速否定：两条规则分别以 `v`、`u` 为目标，一个都没有就直接借用。
    if !bytes.iter().any(|&b| b == b'v' || b == b'u') {
        return Cow::Borrowed(input);
    }
    // 两条规则的触发条件互斥（前驱集合 jqxy 与 nl 不相交），故可以逐字节独立判断，
    // 且判据取**原串**前驱与后继——替换产物不会连锁触发另一条规则。
    let mut out: Option<Vec<u8>> = None;
    for i in 0..bytes.len() {
        let replacement = match bytes[i] {
            b'v' if i > 0 && is_jqxy(bytes[i - 1]) => b'u',
            b'u' if i > 0 && is_nl(bytes[i - 1]) && bytes.get(i + 1) == Some(&b'e') => b'v',
            _ => continue,
        };
        out.get_or_insert_with(|| bytes.to_vec())[i] = replacement;
    }
    match out {
        // 安全性：只把 ASCII 字节换成 ASCII 字节，UTF-8 边界不受影响。
        Some(v) => Cow::Owned(String::from_utf8(v).expect("ASCII 替换不破坏 UTF-8")),
        None => Cow::Borrowed(input),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pinyin::syllable::SyllableTrie;

    #[test]
    fn jqxy_v_becomes_u() {
        for (input, want) in [
            ("xv", "xu"),
            ("jv", "ju"),
            ("qv", "qu"),
            ("yv", "yu"),
            ("jvn", "jun"),
            ("qvan", "quan"),
            ("xve", "xue"),
            ("yvan", "yuan"),
        ] {
            assert_eq!(normalize_u_umlaut(input), want, "输入 {input}");
        }
    }

    #[test]
    fn nl_ue_becomes_ve() {
        assert_eq!(normalize_u_umlaut("nue"), "nve");
        assert_eq!(normalize_u_umlaut("lue"), "lve");
        assert_eq!(normalize_u_umlaut("dalue"), "dalve", "词中同样生效");
    }

    /// n/l 后的 `v` 已经是音节表形态，不得再被动。
    #[test]
    fn nl_v_is_left_alone() {
        for s in ["nv", "lv", "nve", "lve", "nvhai", "lvyou"] {
            assert!(
                matches!(normalize_u_umlaut(s), Cow::Borrowed(_)),
                "{s} 不该被替换，且应零分配"
            );
        }
    }

    /// jqxy 之外的 `ue`、n/l 之外的 `u` 都不是 ü，不得误伤。
    #[test]
    fn unrelated_spellings_untouched() {
        for s in [
            "hao", "guo", "sui", "jue", "xue",
            "yue", // jqxy 后的 ue 本就是正字法形态
            "duo", "tuan", "shuo", "wu", "hue", // h 不在 nl 集合里
            "gue", "kue",
        ] {
            assert!(
                matches!(normalize_u_umlaut(s), Cow::Borrowed(_)),
                "{s} 不该被替换"
            );
        }
    }

    /// 首字符是 `v`/`u` 时没有前驱，两条规则都不成立（`v` 不能起音节）。
    #[test]
    fn leading_byte_has_no_predecessor() {
        assert!(matches!(normalize_u_umlaut("v"), Cow::Borrowed(_)));
        assert!(matches!(normalize_u_umlaut("ue"), Cow::Borrowed(_)));
        assert!(matches!(normalize_u_umlaut(""), Cow::Borrowed(_)));
    }

    /// 一串里多处触发须全部替换，且两条规则可以同时出现。
    #[test]
    fn multiple_occurrences_all_replaced() {
        assert_eq!(normalize_u_umlaut("xvxv"), "xuxu");
        assert_eq!(normalize_u_umlaut("nuexv"), "nvexu");
    }

    /// ★ 归一化的产物必须落在标准音节表里——否则这层白做。
    #[test]
    fn normalized_results_are_real_syllables() {
        let trie = SyllableTrie::new();
        for raw in ["xv", "jv", "qv", "yv", "jvn", "qvan", "xve", "nue", "lue"] {
            let norm = normalize_u_umlaut(raw);
            assert!(!trie.is_syllable(raw), "{raw} 归一化前本就不是音节");
            assert!(trie.is_syllable(&norm), "{raw} → {norm} 应是合法音节");
        }
    }

    /// 等长不变量：位置映射依赖它，破坏了会让 preedit 与 consumed_length 整体错位。
    #[test]
    fn replacement_preserves_length() {
        for s in ["xv", "nue", "nuexvlue", "wonuexvhao"] {
            assert_eq!(normalize_u_umlaut(s).len(), s.len(), "{s} 长度须不变");
        }
    }
}

#[cfg(test)]
mod exhaustiveness {
    use super::*;
    use crate::pinyin::syllable::{STANDARD_SYLLABLES, SyllableTrie};

    /// ★★ **穷尽性自证**：标准音节表里 ü 的落点只有两类，本模块的两条规则各覆盖一类。
    ///
    /// 这条不是在测 `normalize_u_umlaut`，而是在锁**它的前提**——音节表若日后新增
    /// 一个含 `v` 或 jqxy+u 的音节，规则的覆盖面就不再完整，而那不会有任何编译错误。
    /// 断言写成「集合完全相等」而不是「包含」，正是为了让新增项立刻红。
    #[test]
    fn umlaut_landing_sites_in_syllable_table_are_fully_covered() {
        let with_v: Vec<&str> = STANDARD_SYLLABLES
            .iter()
            .copied()
            .filter(|s| s.contains('v'))
            .collect();
        assert_eq!(
            with_v,
            ["lv", "lve", "nv", "nve"],
            "含 `v` 的音节全集变了 ⇒ 归一化规则的覆盖面须重新推导"
        );

        let jqxy_u: Vec<&str> = STANDARD_SYLLABLES
            .iter()
            .copied()
            .filter(|s| {
                let b = s.as_bytes();
                b.len() >= 2 && is_jqxy(b[0]) && b[1] == b'u'
            })
            .collect();
        assert_eq!(
            jqxy_u,
            [
                "ju", "juan", "jue", "jun", "qu", "quan", "que", "qun", "xu", "xuan", "xue", "xun",
                "yu", "yuan", "yue", "yun"
            ],
            "jqxy+u 的音节全集变了 ⇒ 同上"
        );

        // 每一个 jqxy+u 音节，把 `u` 写成 `v` 后都必须能归一回来。
        for syl in &jqxy_u {
            let typed = format!("{}v{}", &syl[..1], &syl[2..]);
            assert_eq!(
                normalize_u_umlaut(&typed),
                *syl,
                "`{typed}` 应归一回 `{syl}`"
            );
        }
    }

    /// `ue` → `ve` 规则的**无损性**：n/l 后不存在以 `ue` 起头的合法音节，
    /// 故这条规则不可能夺走任何原本有解的单音节切分。
    #[test]
    fn ue_rule_steals_no_valid_syllable() {
        let trie = SyllableTrie::new();
        for initial in ["n", "l"] {
            let victim = format!("{initial}ue");
            assert!(
                !trie.is_syllable(&victim),
                "`{victim}` 若成了合法音节，`ue`→`ve` 就变成有损改写，规则须重新论证"
            );
        }
        // 反向：归一化的目标必须真的存在，否则这条规则是空转。
        assert!(trie.is_syllable("nve") && trie.is_syllable("lve"));
    }

    /// 归一化对**任何**标准音节都是恒等的——音节表里的串本就是正字法形态。
    /// 这条一旦红，说明规则误伤了合法拼写（比如错加「n/l 后 v→u」那一臂）。
    #[test]
    fn every_standard_syllable_is_a_fixed_point() {
        for syl in STANDARD_SYLLABLES {
            assert!(
                matches!(normalize_u_umlaut(syl), Cow::Borrowed(_)),
                "`{syl}` 是标准音节，归一化须恒等"
            );
        }
    }
}
