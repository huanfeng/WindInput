use super::*;

/// 锁 API 本身报错（网络家目录上的 flock 等）⇒ 退化为只读，不让整个用户数据不可用。
#[test]
fn lock_api_error_degrades_to_readonly() {
    let d = std::env::temp_dir().join(format!("wind_lockerr-{}", std::process::id()));
    let _ = std::fs::create_dir_all(&d);
    let f = std::fs::File::create(d.join("lock")).unwrap();
    let e = std::fs::TryLockError::Error(std::io::Error::other("ENOLCK"));
    assert!(lock_outcome(Err(e), f).is_none());
    let f = std::fs::File::create(d.join("lock")).unwrap();
    assert!(lock_outcome(Err(std::fs::TryLockError::WouldBlock), f).is_none());
    let f = std::fs::File::create(d.join("lock")).unwrap();
    assert!(lock_outcome(Ok(()), f).is_some());
}

/// Review Focus 5：目录被复制到另一台机器 ⇒ 换新 device_id，旧日志封存，不与新日志混号。
#[test]
fn copied_dir_gets_new_device_and_seals_old_journal() {
    let d = std::env::temp_dir().join(format!("wind_dev-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(d.join("journal")).unwrap();
    let DeviceOutcome::Renewed {
        new: first,
        sealed_journal: None,
    } = load_or_renew(&d, "dir-A", &d.join("journal")).unwrap()
    else {
        panic!("首次应生成")
    };
    assert!(
        matches!(load_or_renew(&d, "dir-A", &d.join("journal")).unwrap(), DeviceOutcome::Same(ref x) if x.device_id == first.device_id),
        "同机同目录：不变"
    );
    // 模拟「目录被复制到另一台机器」：device.toml 里的 machine_fp 对不上
    let text = std::fs::read_to_string(d.join("device.toml"))
        .unwrap()
        .replace(&machine_fingerprint(), "other-machine");
    std::fs::write(d.join("device.toml"), text).unwrap();
    let jnl = d
        .join("journal")
        .join(format!("{}.wjl", hex(&first.device_id)));
    std::fs::write(&jnl, b"WJNL").unwrap();
    let DeviceOutcome::Renewed {
        new,
        sealed_journal,
    } = load_or_renew(&d, "dir-A", &d.join("journal")).unwrap()
    else {
        panic!("指纹不符应换号")
    };
    assert_ne!(new.device_id, first.device_id);
    let sealed = jnl.with_extension("wjl.sealed");
    assert_eq!(sealed_journal.as_deref(), Some(sealed.as_path()));
    assert!(!jnl.exists() && sealed.exists());
    // 换了用户目录（dir_id 不同）同样换号
    assert!(matches!(
        load_or_renew(&d, "dir-B", &d.join("journal")).unwrap(),
        DeviceOutcome::Renewed { .. }
    ));
}

#[test]
fn second_lock_holder_gets_none() {
    let d = std::env::temp_dir().join(format!("wind_lock-{}", std::process::id()));
    let _ = std::fs::create_dir_all(&d);
    let a = try_lock(&d).unwrap();
    assert!(a.is_some());
    assert!(
        try_lock(&d).unwrap().is_none(),
        "同进程再锁一次也该拿不到（File::try_lock 按打开的文件描述计）"
    );
    drop(a);
    assert!(try_lock(&d).unwrap().is_some());
}
