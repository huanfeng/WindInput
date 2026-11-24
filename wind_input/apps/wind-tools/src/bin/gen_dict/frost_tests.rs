use super::*;
use std::io::Write;

fn e(text: &str, code: &str, weight: i64) -> Entry {
    Entry::new(text.into(), code.into(), weight, 0)
}

#[test]
fn generates_only_missing_high_frequency_words_with_full_codes() {
    let path = std::env::temp_dir().join(format!("wind-frost-{}.yaml", std::process::id()));
    let mut file = std::fs::File::create(&path).unwrap();
    write!(file, "---\nname: base\n...\n招行\tzhao hang\t1348\n招行\tzhao hang\t2000\n招招\tzhao zhao\t1000\n行行\thang hang\t999\n招鑫\tzhao xin\t1000\n招A\tzhao a\t1000\n").unwrap();
    drop(file);
    let main = [
        e("招", "rvk", 100),
        e("招", "rvkg", 10),
        e("行", "tfhh", 10),
    ];
    let extra = [e("招招", "rvrv", 10)];
    let result = generate(&path, &main, &extra, 120).unwrap();
    std::fs::remove_file(path).unwrap();
    assert_eq!(result.len(), 1);
    assert_eq!((&result[0].text[..], &result[0].code[..]), ("招行", "rvtf"));
    assert!((1..120).contains(&result[0].weight));
}

#[test]
fn complete_existing_dictionary_produces_an_empty_supplement() {
    let path =
        std::env::temp_dir().join(format!("wind-frost-complete-{}.yaml", std::process::id()));
    std::fs::write(&path, "---\nname: base\n...\n招行\tzhao hang\t1348\n").unwrap();
    let result = generate(&path, &[e("招行", "rvtf", 100)], &[], 120).unwrap();
    std::fs::remove_file(path).unwrap();
    assert!(result.is_empty());
}

#[test]
fn normalizes_bom_and_lone_cr_line_endings() {
    let path = std::env::temp_dir().join(format!("wind-frost-cr-{}.yaml", std::process::id()));
    std::fs::write(
        &path,
        "\u{feff}---\rname: base\r...\r招行\tzhao hang\t1348\r",
    )
    .unwrap();
    let main = [e("招", "rvk", 100), e("行", "tfhh", 10)];
    let result = generate(&path, &main, &[], 120).unwrap();
    std::fs::remove_file(path).unwrap();
    assert_eq!(result.len(), 1);
    assert_eq!((&result[0].text[..], &result[0].code[..]), ("招行", "rvtf"));
}

#[test]
fn invalid_source_is_not_silently_accepted() {
    let path = std::env::temp_dir().join(format!("wind-frost-invalid-{}.yaml", std::process::id()));
    std::fs::write(&path, "---\nname: base\n").unwrap();
    assert!(generate(&path, &[], &[], 120).is_err());
    std::fs::remove_file(path).unwrap();
}
