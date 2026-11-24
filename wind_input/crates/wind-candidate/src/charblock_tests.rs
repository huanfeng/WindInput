use super::*;

/// 表必须按 `start` 升序且互不重叠。
///
/// 有序性是 [`block_index_of`] 二分查找的正确性前提：失序时 `partition_point` 直接返回
/// 错误的块。别当成整洁性检查放宽它。
#[test]
fn blocks_are_sorted_and_disjoint() {
    for w in BLOCKS.windows(2) {
        let (a, b) = (&w[0], &w[1]);
        assert!(a.start <= a.end, "{} 区间反了", a.name);
        assert!(
            a.end < b.start,
            "{} (…{:04X}) 与 {} ({:04X}…) 重叠或失序",
            a.name,
            a.end,
            b.name,
            b.start
        );
    }
}

/// 用户常要单独开关的那几类，块名必须认得出来。
#[test]
fn classifies_the_blocks_from_issue_83() {
    for (ch, name) in [
        ('⿰', "表意文字描述符"),
        ('⿿', "表意文字描述符"),
        ('ㄅ', "注音符号"),
        ('ㆠ', "注音符号扩展"),
        ('あ', "平假名"),
        ('、', "CJK 符号和标点"),
        ('㈱', "带圈 CJK 字母及月份"),
        // ℃ 是 U+2103「字母式符号」，不是 CJK 兼容符号——后者是 ㎡(U+33A1) 那一类。
        // 两者在候选里长得像「同一种东西」，块归属却隔着一万多个码位。
        ('℃', "字母式符号"),
        ('㎡', "CJK 兼容符号"),
        ('😀', "表情符号"),
        // 部首块里的三点水是 ⺡(U+2EA1)；日常打出来的 氵(U+6C35) 是基本汉字。
        // 两个字形一模一样，块归属不同——按字形猜块必错。
        ('⺡', "CJK 部首补充"),
        ('氵', "基本汉字"),
        ('⼀', "康熙部首"),
        ('㇀', "CJK 笔画"),
        ('我', "基本汉字"),
        ('\u{3400}', "扩展 A"),
        ('\u{20000}', "扩展 B"),
        ('\u{2EBF0}', "扩展 I"),
        ('\u{323B0}', "扩展 J"),
        ('\u{E831}', "私用区"),
    ] {
        assert_eq!(block_of(ch).name, name, "{ch} 归类错了");
    }
    assert_eq!(block_of('\u{0}').name, "其它");
}

/// 批量操作的闸门：域外块开放，默认字表管得着的块一律关闭。
///
/// 「基本汉字」这条是本测试的重点——放行它等于给出一个一键作废 7831 条默认判定的按钮。
#[test]
fn bulk_edit_only_for_blocks_outside_the_default_table() {
    for ch in ['ㄅ', 'ㆠ', '⿰', 'あ', '、', '㈱', '℃'] {
        assert!(
            block_allows_bulk_edit(&block_of(ch)),
            "{ch} 所在块应开放批量"
        );
    }
    for ch in [
        '我',
        '\u{3400}',
        '\u{20000}',
        '\u{323B0}',
        '氵',
        '⼀',
        '㇀',
        '\u{E831}',
    ] {
        assert!(
            !block_allows_bulk_edit(&block_of(ch)),
            "{ch} 所在块受默认字表管辖，必须禁止批量"
        );
    }
    // OTHER 是空区间，批量无意义。
    assert!(!block_allows_bulk_edit(&block_of('\u{0}')));
}

#[test]
fn range_text_is_hex_padded() {
    assert_eq!(block_of('⿰').range_text(), "2FF0-2FFF");
    assert_eq!(block_of('\u{323B0}').range_text(), "323B0-3347F");
}

/// 早退用的下标必须真的指向「基本汉字」。
///
/// 往表中插块会让后续下标平移，下标错了整片汉字会静默归进邻近块。常量由编译期扫表
/// 算出，这里钉住扫表判据（`start == 0x4E00`）没有失配。
#[test]
fn basic_han_index_points_at_the_right_block() {
    assert_eq!(BLOCKS[BASIC_HAN_IDX].name, "基本汉字");
    assert_eq!(block_of('我').name, "基本汉字");
    assert_eq!(block_of('\u{4E00}').name, "基本汉字");
    assert_eq!(block_of('\u{9FFF}').name, "基本汉字");
}

/// 二分 + 早退必须与线性扫描逐字符等价：码位静默换归类会让准入判据把一批字挡掉。
/// 不抽样，遍历每个块的两端与块间空隙。
#[test]
fn binary_search_matches_linear_scan() {
    fn linear(ch: char) -> CharBlock {
        let c = ch as u32;
        for blk in BLOCKS {
            if blk.start <= c && c <= blk.end {
                return *blk;
            }
        }
        OTHER
    }

    let mut probes: Vec<u32> = vec![0, 0x1F, 0x10FFFF];
    for blk in BLOCKS {
        // 块内两端与中点，以及紧邻两侧的空隙码位。
        probes.extend([
            blk.start.saturating_sub(1),
            blk.start,
            blk.start + (blk.end - blk.start) / 2,
            blk.end,
            blk.end + 1,
        ]);
    }
    for c in probes {
        let Some(ch) = char::from_u32(c) else {
            continue; // 代理区等非法标量值，两条路径都够不着
        };
        assert_eq!(
            block_of(ch).name,
            linear(ch).name,
            "U+{c:04X} 的归类被算法替换改变了"
        );
    }
}

/// 国旗的两个区域指示符必须各自归到「区域指示符」块。
///
/// 落在「其它」的话 emoji 预设组带不出国旗。
#[test]
fn regional_indicators_are_classified() {
    assert_eq!(block_of('\u{1F1E8}').name, "区域指示符"); // 🇨
    assert_eq!(block_of('\u{1F1F3}').name, "区域指示符"); // 🇳
    assert_eq!(block_of_cluster("🇨🇳").name, "区域指示符");
    // 邻接的「表情符号」块不受影响。
    assert_eq!(block_of('😀').name, "表情符号");
}
