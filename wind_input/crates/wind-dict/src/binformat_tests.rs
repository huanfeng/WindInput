use super::*;

/// 往返测试：写入多 key、每个 key 多条目，确保【非首 key】也能读对。
/// 这是 entry_off 字节偏移语义的回归保护——历史 bug 是写入器把 entry_off
/// 写成了累计条目数（count）而非字节偏移（count × entry_size），导致除首
/// key 外的所有词条读到错误位置（text 为空/乱码），五笔与拼音候选全废。
#[test]
fn test_writer_reader_roundtrip_multi_key() {
    let tmp = std::env::temp_dir().join("wind_dict_roundtrip_test.wdb");

    let mut writer = DictWriter::new();
    // 故意让第一个 key 有 2 条，使后续 key 的 entry_off > 0（暴露 off-by-size bug）
    writer.add(
        "a".to_string(),
        vec![("工".to_string(), 9999), ("戈".to_string(), 100)],
    );
    writer.add(
        "ni".to_string(),
        vec![("你".to_string(), 800), ("尼".to_string(), 50)],
    );
    writer.add("nihao".to_string(), vec![("你好".to_string(), 1200)]);
    writer.add("zhongguo".to_string(), vec![("中国".to_string(), 2000)]);
    writer.write(&tmp).expect("write wdb");

    let reader = DictReader::open(&tmp).expect("open wdb");
    assert_eq!(reader.key_count(), 4);

    // 首 key（entry_off=0）
    let a = reader.search("a");
    assert_eq!(a.len(), 2);
    assert!(a.iter().any(|e| e.text == "工" && e.weight == 9999));

    // 非首 key（entry_off > 0）——历史 bug 在此读到乱码
    let nihao = reader.search("nihao");
    assert_eq!(nihao.len(), 1, "nihao 应有 1 条候选");
    assert_eq!(nihao[0].text, "你好");
    assert_eq!(nihao[0].weight, 1200);

    let ni = reader.search("ni");
    assert_eq!(ni.len(), 2);
    assert!(ni.iter().any(|e| e.text == "你" && e.weight == 800));
    assert!(ni.iter().any(|e| e.text == "尼"));

    let zg = reader.search("zhongguo");
    assert_eq!(zg.len(), 1);
    assert_eq!(zg[0].text, "中国");

    // 前缀查找：ni 前缀应命中 ni / nihao
    let prefix = reader.search_prefix("ni", 10);
    assert!(prefix.iter().any(|e| e.text == "你好"));
    assert!(prefix.iter().any(|e| e.text == "你"));

    let _ = std::fs::remove_file(&tmp);
}

/// **词条文本的尾随空白必须原样穿过缓存**。
///
/// 解析层已改为保留末列 text 的尾随空格（蒙古文以空格作词间分隔，toli 词库整库如此，
/// 见 `codetable::trim_line_end`）；但源文件解析对了、缓存这一段再吃掉，用户看到的
/// 依然是丢空格——而且只在第二次启动（命中缓存）时复现，最难排查。
///
/// 本格式用长度前缀存串（非 NUL 结尾、非按空白切分），故理应无损；此处把它钉死。
#[test]
fn test_roundtrip_preserves_trailing_whitespace_in_text() {
    let tmp = std::env::temp_dir().join("wind_dict_trailing_ws.wdb");

    let mut writer = DictWriter::new();
    // 蒙古文形态：PUA 码位 + 内部空格（词间分隔）+ 尾随空格
    writer.add(
        "aabn".to_string(),
        vec![("\u{e226}\u{e2f4} \u{e341}\u{e2ca} ".to_string(), 0)],
    );
    // 同一 code 下「有尾随空格」与「无尾随空格」是两条不同词条，不得被并成一条
    writer.add(
        "ka".to_string(),
        vec![("甲 ".to_string(), 10), ("甲".to_string(), 5)],
    );
    writer.write(&tmp).expect("write wdb");

    let reader = DictReader::open(&tmp).expect("open wdb");
    let m = reader.search("aabn");
    assert_eq!(m.len(), 1);
    assert_eq!(
        m[0].text, "\u{e226}\u{e2f4} \u{e341}\u{e2ca} ",
        "内部空格与尾随空格都须原样穿过缓存"
    );

    let ka = reader.search("ka");
    assert_eq!(ka.len(), 2, "差一个尾随空格的两条词条须各自独立");
    assert!(ka.iter().any(|e| e.text == "甲 " && e.weight == 10));
    assert!(ka.iter().any(|e| e.text == "甲" && e.weight == 5));

    let _ = std::fs::remove_file(&tmp);
}

/// for_each_entry 应枚举全部 (code,text,weight),供反查索引构建(mmap 路径)。
#[test]
fn test_for_each_entry_enumerates_all() {
    let tmp = std::env::temp_dir().join("wind_dict_foreach_test.wdb");
    let mut writer = DictWriter::new();
    writer.add(
        "a".to_string(),
        vec![("工".to_string(), 9999), ("戈".to_string(), 100)],
    );
    writer.add("aaaa".to_string(), vec![("工".to_string(), 50)]);
    writer.write(&tmp).expect("write wdb");

    let reader = DictReader::open(&tmp).expect("open wdb");
    let mut got: Vec<(String, String, i32)> = Vec::new();
    reader.for_each_entry(&mut |c, t, w| got.push((c.to_string(), t.to_string(), w)));
    assert_eq!(got.len(), 3, "应枚举 3 条");
    assert!(got.contains(&("a".to_string(), "工".to_string(), 9999)));
    assert!(got.contains(&("a".to_string(), "戈".to_string(), 100)));
    assert!(got.contains(&("aaaa".to_string(), "工".to_string(), 50)));

    let _ = std::fs::remove_file(&tmp);
}
