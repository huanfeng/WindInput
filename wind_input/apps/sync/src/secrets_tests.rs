use super::*;

#[test]
fn roundtrip_and_clear() {
    let d = tempfile::tempdir().unwrap();
    let st = FileSecretStore::new(d.path().join("sync").join(FILE_NAME));
    assert!(Secrets::load(&st).unwrap().is_empty());
    let s = Secrets {
        dek: Some("AAAA".into()),
        password: Some("pw".into()),
    };
    s.save(&st).unwrap();
    assert_eq!(Secrets::load(&st).unwrap(), s);
    Secrets::default().save(&st).unwrap();
    assert!(Secrets::load(&st).unwrap().is_empty());
    assert!(st.read().unwrap().is_none());
}

#[cfg(unix)]
#[test]
fn file_is_private_from_creation_and_after_rewrite() {
    use std::os::unix::fs::PermissionsExt;
    let d = tempfile::tempdir().unwrap();
    let p = d.path().join(FILE_NAME);
    let st = FileSecretStore::new(p.clone());
    st.write(b"a").unwrap();
    let mode = |p: &Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode(&p), 0o600);
    st.write(b"b").unwrap();
    assert_eq!(mode(&p), 0o600);
    assert_eq!(st.read().unwrap().unwrap(), b"b");
}

#[test]
fn corrupt_content_is_an_error_not_empty() {
    let d = tempfile::tempdir().unwrap();
    let st = FileSecretStore::new(d.path().join(FILE_NAME));
    st.write(b"not json").unwrap();
    assert!(Secrets::load(&st).is_err());
}

#[test]
fn debug_does_not_leak() {
    let s = Secrets {
        dek: Some("TOPSECRET".into()),
        password: Some("hunter2".into()),
    };
    let t = format!("{s:?}");
    assert!(!t.contains("TOPSECRET") && !t.contains("hunter2"));
}

#[cfg(windows)]
#[test]
fn dpapi_roundtrip_stores_no_plaintext_and_rejects_tampering() {
    let d = tempfile::tempdir().unwrap();
    let path = d.path().join("sync").join(FILE_NAME);
    let st = default_store(d.path().join("sync").as_path());
    let s = Secrets {
        dek: Some("ZGVrLXBsYWludGV4dA==".into()),
        password: Some("webdav-pass-123".into()),
    };
    s.save(st.as_ref()).unwrap();
    assert_eq!(Secrets::load(st.as_ref()).unwrap(), s);
    let raw = std::fs::read(&path).unwrap();
    let hay = String::from_utf8_lossy(&raw);
    assert!(!hay.contains("webdav-pass-123") && !hay.contains("ZGVrLXBsYWludGV4dA"));
    let mut bad = raw.clone();
    let n = bad.len();
    bad[n - 1] ^= 0xff;
    std::fs::write(&path, bad).unwrap();
    assert!(Secrets::load(st.as_ref()).is_err());
}
