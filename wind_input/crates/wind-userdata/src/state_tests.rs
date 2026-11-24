use super::*;

#[test]
fn state_roundtrip_and_corrupt_file_reads_as_unknown() {
    let d = std::env::temp_dir().join(format!("wind_state-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    let p = d.join("state");
    let mut s = State {
        version: STATE_VERSION,
        generation: 2,
        next_seq: 10,
        targets: Default::default(),
    };
    s.targets.insert(
        "temp:pinyin".into(),
        TargetState {
            folded_seq: 9,
            text_fp: None,
        },
    );
    s.save(&p).unwrap();
    assert_eq!(State::load(&p), Some(s));
    std::fs::write(&p, "garbage ==").unwrap();
    assert_eq!(State::load(&p), None, "损坏 ⇒ 水位未知，由快照内嵌水位兜底");
    assert!(
        std::fs::read_dir(&d)
            .unwrap()
            .flatten()
            .all(|e| !e.file_name().to_string_lossy().contains(".tmp")),
        "tmp 不残留"
    );
}

#[test]
fn atomic_write_replaces_whole_file() {
    let d = std::env::temp_dir().join(format!("wind_aw-{}", std::process::id()));
    let _ = std::fs::create_dir_all(&d);
    let p = d.join("f.bin");
    atomic_write(&p, b"long old content").unwrap();
    atomic_write(&p, b"new").unwrap();
    assert_eq!(std::fs::read(&p).unwrap(), b"new");
}

#[test]
fn state_with_unknown_version_is_treated_as_missing() {
    let d = std::env::temp_dir().join(format!("wind_state_ver-{}", std::process::id()));
    let _ = std::fs::create_dir_all(&d);
    let p = d.join("state");
    let s = State {
        next_seq: 9,
        ..Default::default()
    };
    s.save(&p).unwrap();
    let back = State::load(&p).expect("同版本可读");
    assert_eq!((back.version, back.next_seq), (STATE_VERSION, 9));
    let text = std::fs::read_to_string(&p)
        .unwrap()
        .replace(&format!("version = {STATE_VERSION}"), "version = 99");
    std::fs::write(&p, text).unwrap();
    assert!(State::load(&p).is_none(), "版本不符 = 水位未知");
    let _ = std::fs::remove_dir_all(&d);
}
