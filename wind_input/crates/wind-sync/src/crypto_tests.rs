use super::*;

#[test]
fn roundtrip_and_aad_binding() {
    let d = Dek::generate().files_key();
    let c = seal(&d, ObjKind::Blob, "a", b"hello");
    assert_eq!(open(&d, ObjKind::Blob, "a", &c).unwrap(), b"hello");
    assert!(open(&d, ObjKind::Blob, "b", &c).is_err());
    assert!(open(&d, ObjKind::Manifest, "a", &c).is_err());
    assert!(open(&Dek::generate().files_key(), ObjKind::Blob, "a", &c).is_err());
    let mut bad = c.clone();
    *bad.last_mut().unwrap() ^= 1;
    assert!(open(&d, ObjKind::Blob, "a", &bad).is_err());
}

#[test]
fn nonce_is_random() {
    let d = Dek::generate().files_key();
    assert_ne!(
        seal(&d, ObjKind::Blob, "a", b"x"),
        seal(&d, ObjKind::Blob, "a", b"x")
    );
}

#[test]
fn meta_verify_right_and_wrong_key() {
    let d = Dek::generate();
    let m = Meta::from_json(&Meta::new(&d).to_json()).unwrap();
    assert_eq!(m.format, 2);
    assert!(m.verify(&d));
    assert!(!m.verify(&Dek::generate()));
}

#[test]
fn unknown_meta_format_is_unsupported() {
    let v3 = br#"{"format":3,"check":"x"}"#;
    assert_eq!(Meta::from_json(v3), Err(CryptoError::UnsupportedMeta));
    assert_eq!(Meta::from_json(b"junk"), Err(CryptoError::UnsupportedMeta));
}

#[test]
fn key_string_roundtrip_shape_and_tolerant_parse() {
    let d = Dek::generate();
    let s = d.to_key_string();
    assert_eq!(s.len(), 52 + 12);
    assert!(s.split('-').all(|g| g.len() == 4));
    assert_eq!(s, s.to_lowercase());
    assert_eq!(Dek::from_key_string(&s).unwrap(), d);
    let messy = format!("  {}\n--{} ", s.to_uppercase().replace('-', " - "), "");
    assert_eq!(Dek::from_key_string(&messy).unwrap(), d);
    assert_eq!(Dek::from_key_string(&s.replace('-', "")).unwrap(), d);
}

#[test]
fn key_string_rejects_bad_length_and_alphabet() {
    let s = Dek::generate().to_key_string();
    assert!(Dek::from_key_string("").is_err());
    assert!(Dek::from_key_string(&s[..s.len() - 5]).is_err());
    assert!(Dek::from_key_string(&format!("{s}aaaa")).is_err());
    // base32 字母表不含 1 8 9 0
    assert!(Dek::from_key_string(&s.replacen(&s[..1], "1", 1)).is_err());
    assert!(Dek::from_key_string(&"0".repeat(52)).is_err());
}

#[test]
fn dek_base64_roundtrip_and_blob_id_stable() {
    let d = Dek::generate();
    assert_eq!(Dek::from_base64(&d.to_base64()).unwrap(), d);
    assert!(Dek::from_base64("abc").is_err());
    let h = content_hash(b"x");
    assert_eq!(blob_id(&d.files_key(), &h), blob_id(&d.files_key(), &h));
    assert_ne!(
        blob_id(&d.files_key(), &h),
        blob_id(&Dek::generate().files_key(), &h)
    );
}

#[test]
fn key_string_rejects_noncanonical_trailing_bits_and_accepts_wide_separators() {
    let d = Dek::generate();
    let s = d.to_key_string().replace('-', "");
    // 最后一个字符只有高 4 位有效：换成同高位、低位不同的字符即非规范编码
    let alphabet = "abcdefghijklmnopqrstuvwxyz234567";
    let last = s.chars().last().unwrap();
    let idx = alphabet.find(last).unwrap();
    assert_eq!(idx & 0b1111, 0, "规范编码的低 4 位为 0");
    let tampered = format!("{}{}", &s[..51], alphabet.chars().nth(idx | 1).unwrap());
    assert!(Dek::from_key_string(&tampered).is_err());
    // 全角 / 长连字符与全角空格
    let wide: String = s
        .as_bytes()
        .chunks(4)
        .map(|c| std::str::from_utf8(c).unwrap())
        .collect::<Vec<_>>()
        .join("－");
    assert_eq!(Dek::from_key_string(&wide).unwrap(), d);
    let mixed = format!(
        "{}—{}\u{3000}{}–{}",
        &s[..8],
        &s[8..20],
        &s[20..40],
        &s[40..]
    );
    assert_eq!(Dek::from_key_string(&mixed).unwrap(), d);
    // 长度错有自己的提示
    match Dek::from_key_string(&s[..51]) {
        Err(CryptoError::Format(m)) => assert!(m.contains("长度"), "{m}"),
        o => panic!("{o:?}"),
    }
}

const ALL: [Purpose; 5] = [
    Purpose::Files,
    Purpose::DevInfo,
    Purpose::Clip,
    Purpose::Xfer,
    Purpose::Lan,
];

#[test]
fn subkeys_are_pairwise_distinct_and_deterministic() {
    let d = Dek::generate();
    for (i, a) in ALL.iter().enumerate() {
        assert_eq!(d.derive(*a), d.derive(*a));
        for b in &ALL[i + 1..] {
            assert_ne!(d.derive(*a), d.derive(*b), "{a:?} 与 {b:?} 派生结果相同");
        }
    }
    assert_ne!(
        d.derive(Purpose::Files),
        Dek::generate().derive(Purpose::Files)
    );
}

#[test]
fn same_dek_on_two_devices_derives_same_subkeys() {
    let a = Dek::generate();
    let b = Dek::from_base64(&a.to_base64()).unwrap();
    for p in ALL {
        assert_eq!(a.derive(p), b.derive(p));
    }
    let c = seal(&a.files_key(), ObjKind::Blob, "x", b"hi");
    assert_eq!(open(&b.files_key(), ObjKind::Blob, "x", &c).unwrap(), b"hi");
}

#[test]
fn object_sealed_for_one_purpose_cannot_be_opened_by_another() {
    let d = Dek::generate();
    let c = seal(&d.devinfo_key(), ObjKind::DevInfo, "dev1", b"info");
    assert_eq!(
        open(&d.devinfo_key(), ObjKind::DevInfo, "dev1", &c).unwrap(),
        b"info"
    );
    assert!(open(&d.files_key(), ObjKind::DevInfo, "dev1", &c).is_err());
    assert!(open(&d.clip_key(), ObjKind::DevInfo, "dev1", &c).is_err());
    assert!(open(&d.devinfo_key(), ObjKind::Manifest, "dev1", &c).is_err());
    assert!(open(&d.devinfo_key(), ObjKind::DevInfo, "dev2", &c).is_err());
}
