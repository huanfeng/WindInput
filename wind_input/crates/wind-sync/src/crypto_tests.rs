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
fn meta_unlock_and_wrong_pass() {
    let d = Dek::generate();
    let m = Meta::new(&d, "pw", KdfParams::FAST_TEST).unwrap();
    let m2 = Meta::from_json(&m.to_json()).unwrap();
    assert_eq!(m2.unlock("pw").unwrap(), d);
    assert_eq!(m2.unlock("nope"), Err(CryptoError::BadPassphrase));
    assert!(m2.check_dek(&d));
    assert!(!m2.check_dek(&Dek::generate()));
}

#[test]
fn same_pass_different_salt() {
    let d = Dek::generate();
    let a = Meta::new(&d, "pw", KdfParams::FAST_TEST).unwrap();
    let b = Meta::new(&d, "pw", KdfParams::FAST_TEST).unwrap();
    assert_ne!(a.salt, b.salt);
    assert_ne!(a.wrapped_dek, b.wrapped_dek);
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
fn hostile_kdf_params_rejected() {
    let d = Dek::generate();
    let mut m = Meta::new(&d, "pw", KdfParams::FAST_TEST).unwrap();
    m.kdf.m_kib = u32::MAX;
    assert!(Meta::from_json(&m.to_json()).is_err());
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
