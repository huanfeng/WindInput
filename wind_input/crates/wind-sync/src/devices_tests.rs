use super::*;
use crate::backend::MemBackend;
use crate::crypto::Dek;

fn sample() -> DeviceInfo {
    DeviceInfo {
        name: "n".into(),
        platform: "windows".into(),
        app_version: "1.2.3".into(),
        caps: vec!["files".into()],
        last_seen: 42,
    }
}

#[test]
fn put_then_list_roundtrip() {
    let b = MemBackend::new();
    let k = Dek::generate().devinfo_key();
    put_device_info(&b, &k, "d1", &sample()).unwrap();
    let l = list_devices(&b, &k).unwrap();
    assert_eq!(l.len(), 1);
    assert_eq!(l[0].0, "d1");
    assert_eq!(l[0].1.as_ref().unwrap(), &sample());
}

#[test]
fn foreign_key_makes_info_unreadable_not_the_listing_fail() {
    let b = MemBackend::new();
    put_device_info(&b, &Dek::generate().devinfo_key(), "d1", &sample()).unwrap();
    let l = list_devices(&b, &Dek::generate().devinfo_key()).unwrap();
    assert!(l[0].1.is_err());
}

#[test]
fn info_moved_to_another_device_id_is_rejected() {
    let b = MemBackend::new();
    let k = Dek::generate().devinfo_key();
    put_device_info(&b, &k, "d1", &sample()).unwrap();
    let raw = b.raw_get(&manifest::device_info_key("d1")).unwrap();
    b.raw_put(&manifest::device_info_key("d2"), raw);
    let l = list_devices(&b, &k).unwrap();
    assert!(l.iter().find(|(d, _)| d == "d1").unwrap().1.is_ok());
    assert!(l.iter().find(|(d, _)| d == "d2").unwrap().1.is_err());
}

#[test]
fn files_subkey_cannot_open_info() {
    let b = MemBackend::new();
    let dek = Dek::generate();
    put_device_info(&b, &dek.devinfo_key(), "d1", &sample()).unwrap();
    let raw = b.raw_get(&manifest::device_info_key("d1")).unwrap();
    assert!(
        crate::crypto::open(
            &dek.files_key(),
            crate::crypto::ObjKind::DevInfo,
            "d1",
            &raw
        )
        .is_err()
    );
}

#[test]
fn remove_only_touches_that_device_and_rejects_bad_ids() {
    let b = MemBackend::new();
    let k = Dek::generate().devinfo_key();
    put_device_info(&b, &k, "ab", &sample()).unwrap();
    put_device_info(&b, &k, "abc", &sample()).unwrap();
    b.raw_put(&manifest::manifest_key("ab", 3), vec![1]);
    b.raw_put(&manifest::blob_key("x"), vec![1]);
    assert_eq!(remove_device(&b, "ab").unwrap(), 2);
    assert_eq!(b.keys(&manifest::devices_prefix()).len(), 1);
    assert_eq!(b.keys(&manifest::blobs_prefix()).len(), 1);
    assert!(remove_device(&b, "../x").is_err());
    assert!(remove_device(&b, "").is_err());
}
