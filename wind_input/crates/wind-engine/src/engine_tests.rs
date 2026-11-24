use super::{AuxCharLookup, aux_code_letter_bit};

/// 只给逐字判定，掩码走默认实现。
struct Slow(Vec<(char, &'static str)>);

impl AuxCharLookup for Slow {
    fn any_code_starts_with(&self, ch: char, prefix: &str) -> bool {
        self.0
            .iter()
            .any(|(c, code)| *c == ch && code.starts_with(prefix))
    }
    fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

/// 同一张表，掩码按码的首字母直接拼。
struct Fast(Slow);

impl AuxCharLookup for Fast {
    fn any_code_starts_with(&self, ch: char, prefix: &str) -> bool {
        self.0.any_code_starts_with(ch, prefix)
    }
    fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
    fn letter_mask(&self, ch: char) -> u32 {
        self.0
            .0
            .iter()
            .filter(|(c, _)| *c == ch)
            .fold(0, |m, (_, code)| m | aux_code_letter_bit(code))
    }
}

#[test]
fn aux_letter_mask_matches_starts_with() {
    let rows = vec![
        ('好', "nz"),
        ('好', "zn"),
        ('是', "uu"),
        ('一', "a"),
        ('乙', "Zy"),
        ('人', "1r"),
    ];
    for table in [Slow(rows.clone()), Slow(Vec::new())] {
        let fast = Fast(Slow(table.0.clone()));
        for ch in ['好', '是', '一', '乙', '人', '无'] {
            let mut buf = [0u8; 4];
            let want = (b'a'..=b'z').enumerate().fold(0u32, |m, (i, b)| {
                let one = char::from(b).encode_utf8(&mut buf);
                m | (u32::from(table.any_code_starts_with(ch, one)) << i)
            });
            assert_eq!(table.letter_mask(ch), want, "默认实现 {ch}");
            assert_eq!(fast.letter_mask(ch), want, "快速实现 {ch}");
        }
    }
    let t = Slow(rows);
    assert_eq!(
        t.letter_mask('好'),
        (1 << (b'n' - b'a')) | (1 << (b'z' - b'a'))
    );
    assert_eq!(t.letter_mask('乙'), 0, "大写开头不算");
    assert_eq!(t.letter_mask('人'), 0, "数字开头不算");
    assert_eq!(t.letter_mask('无'), 0, "无码");
}
