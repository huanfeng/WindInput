use super::*;
use wind_engine::AuxCharLookup;

fn now(table: AuxCodeTable) -> AuxLookupNow {
    AuxLookupNow {
        rt: Arc::new(AuxCodeRuntime {
            key: Vec::new(),
            name: String::new(),
            layers: vec![Layer::Table(table)],
        }),
        views: vec![None],
    }
}

fn check(table_rows: Vec<(char, &str)>) {
    let reference = AuxCodeTable::from_rows(table_rows.clone());
    let lk = now(AuxCodeTable::from_rows(table_rows));
    for (ch, prefix) in [
        ('好', "h"),
        ('好', "hn"),
        ('好', "x"),
        ('无', "h"),
        ('是', "u"),
    ] {
        assert_eq!(
            AuxCharLookup::any_code_starts_with(&lk, ch, prefix),
            AuxCodeLookup::any_code_starts_with(&reference, ch, prefix),
            "{ch} {prefix}"
        );
    }
    assert_eq!(
        AuxCharLookup::is_empty(&lk),
        AuxCodeLookup::is_empty(&reference)
    );
    for ch in ['好', '是', '无'] {
        assert_eq!(lk.letter_mask(ch), default_mask(&lk, ch), "{ch}");
    }
}

/// 掩码的默认实现（逐字母 `any_code_starts_with`），作快速实现的参照。
pub(crate) fn default_mask(lk: &dyn AuxCharLookup, ch: char) -> u32 {
    let mut buf = [0u8; 4];
    (b'a'..=b'z').enumerate().fold(0, |m, (i, b)| {
        let one = char::from(b).encode_utf8(&mut buf);
        m | (u32::from(lk.any_code_starts_with(ch, one)) << i)
    })
}

#[test]
fn matches_aux_code_lookup_on_small_table() {
    check(vec![('好', "hn"), ('好', "nv"), ('是', "uu"), ('是', "Ux")]);
}

#[test]
fn matches_aux_code_lookup_on_empty_table() {
    check(vec![]);
}

#[test]
fn is_send_sync_trait_object() {
    let arc: Arc<dyn AuxCharLookup> = Arc::new(now(AuxCodeTable::from_rows(vec![('好', "hn")])));
    assert!(arc.any_code_starts_with('好', "h"));
}
