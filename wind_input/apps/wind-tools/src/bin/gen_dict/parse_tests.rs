use super::*;
use std::io::Write;

fn write_temp(name: &str, content: &str) -> std::path::PathBuf {
    let p = std::env::temp_dir().join(format!("gen_dict_test_{name}"));
    let mut f = std::fs::File::create(&p).unwrap();
    f.write_all(content.as_bytes()).unwrap();
    p
}

#[test]
fn columns_declaration_overrides_default_order() {
    // 声明 code 在前、text 在后 —— 与默认顺序相反
    let p = write_temp(
        "cols.yaml",
        "---\nname: t\ncolumns:\n  - code\n  - text\n  - weight\n...\nabcd\t中\t500\n",
    );
    let e = parse_jidian(&p).unwrap();
    assert_eq!(e.len(), 1);
    assert_eq!(e[0].text, "中", "应按列名而非位置取值");
    assert_eq!(e[0].code, "abcd");
    assert_eq!(e[0].weight, 500);
}

#[test]
fn missing_columns_uses_rime_default_text_code_weight() {
    let p = write_temp("nocols.yaml", "---\nname: t\n...\n中\tabcd\t500\n");
    let e = parse_jidian(&p).unwrap();
    assert_eq!(e[0].text, "中");
    assert_eq!(e[0].code, "abcd");
}

#[test]
fn column_names_strip_inline_comments() {
    let p = write_temp(
        "cmt.yaml",
        "---\ncolumns:\n  - code # 编码\n  - text # 词条\n...\nabcd\t中\n",
    );
    let e = parse_jidian(&p).unwrap();
    assert_eq!(e[0].code, "abcd");
    assert_eq!(e[0].text, "中");
    assert_eq!(e[0].weight, 10, "无 weight 列时取缺省优先级 10");
}

#[test]
fn header_content_is_never_parsed_as_data() {
    // `...` 之前的任何 TAB 行都属头部
    let p = write_temp(
        "hdr.yaml",
        "---\nname: t\nnote\tfake\tdata\n...\n中\tabcd\t1\n",
    );
    let e = parse_jidian(&p).unwrap();
    assert_eq!(e.len(), 1, "头部的 TAB 行不得进入词条");
    assert_eq!(e[0].text, "中");
}

#[test]
fn orig_pos_tracks_file_order() {
    let p = write_temp(
        "pos.yaml",
        "---\n...\n甲\taaaa\t1\n乙\tbbbb\t1\n丙\tcccc\t1\n",
    );
    let e = parse_jidian(&p).unwrap();
    assert_eq!(
        e.iter().map(|x| x.orig_pos).collect::<Vec<_>>(),
        vec![0, 1, 2]
    );
}

#[test]
fn extra_uppercase_code_is_normalized() {
    let p = write_temp("up.yaml", "---\n...\napi\tAPI\t5\n");
    let mut logs = Vec::new();
    let e = parse_extra_dict(&p, &CharCodes::new(), &mut |s| logs.push(s)).unwrap();
    assert_eq!(e[0].code, "api", "大写编码应归一为小写");
}

#[test]
fn extra_recovers_code_column_filled_with_text() {
    // "白狐\t白狐" —— code 列被错填成词本身，应按五笔规则重新合成
    let mut cc = CharCodes::new();
    cc.insert('白', "rrrr".into());
    cc.insert('狐', "qtry".into());
    let p = write_temp("bad.yaml", "---\n...\n白狐\t白狐\t5\n");
    let mut logs = Vec::new();
    let e = parse_extra_dict(&p, &cc, &mut |s| logs.push(s)).unwrap();
    assert_eq!(e.len(), 1);
    assert_eq!(e[0].code, "rrqt", "2 字词取各自前 2 码");
    assert!(logs.iter().any(|l| l.contains("修正非法编码")));
}

#[test]
fn extra_drops_unrecoverable_code() {
    let p = write_temp("drop.yaml", "---\n...\n白狐\t白狐\t5\n");
    let mut logs = Vec::new();
    // 空反查表 → 合成不出来
    let e = parse_extra_dict(&p, &CharCodes::new(), &mut |s| logs.push(s)).unwrap();
    assert!(e.is_empty());
    assert!(logs.iter().any(|l| l.contains("跳过非法编码行")));
}

#[test]
fn extra_allows_z_code_unlike_main_dict() {
    // 英文桶的 brz 是合法的；主库的 a-y 校验不适用于 extra
    let p = write_temp("z.yaml", "---\n...\nbrz\tbrz\t5\n");
    let mut logs = Vec::new();
    let e = parse_extra_dict(&p, &CharCodes::new(), &mut |s| logs.push(s)).unwrap();
    assert_eq!(e[0].code, "brz");
}
