use super::*;
use serde_json::json;

fn roundtrip<T: Serialize + for<'de> Deserialize<'de> + PartialEq + std::fmt::Debug>(m: T) {
    let mut buf = Vec::new();
    write_frame(&mut buf, &m).unwrap();
    let back: T = read_frame(&mut buf.as_slice()).unwrap().unwrap();
    assert_eq!(back, m);
}

fn s3() -> SyncSettings {
    SyncSettings {
        enabled: true,
        backend: Some(Backend::S3 {
            endpoint: "https://s3.example.com".into(),
            region: "auto".into(),
            bucket: "b".into(),
            prefix: "p/".into(),
            access_key_id: "AK".into(),
            path_style: true,
        }),
        ..Default::default()
    }
}

#[test]
fn messages_roundtrip() {
    roundtrip(ToSync::Hello {
        suffix: "_dev".into(),
        userdata_dir: "/u".into(),
        state_dir: "/s".into(),
        settings: s3(),
        portable: true,
    });
    roundtrip(ToSync::Settings(SyncSettings::default()));
    roundtrip(ToSync::Command {
        id: 7,
        op: Op::SetSecret,
        args: json!({"password": "x"}),
    });
    roundtrip(ToSync::reply(3, Ok(json!({"ok": true}))));
    roundtrip(ToSync::reply(3, Err("拒绝".into())));
    roundtrip(ToSync::Shutdown);
    roundtrip(FromSync::Status(Status {
        phase: "idle".into(),
        last_ok: Some(1),
        ..Default::default()
    }));
    roundtrip(FromSync::Request {
        id: 1,
        method: "dict.reload".into(),
        params: json!({}),
    });
    roundtrip(FromSync::command_reply(2, Ok(json!({}))));
}

#[test]
fn status_without_conflict_paths_still_decodes() {
    let st: Status = serde_json::from_str(r#"{"phase":"idle","conflicts":2}"#).unwrap();
    assert_eq!(st.conflicts, 2);
    assert!(st.conflict_paths.is_empty());
}

#[test]
fn wire_names_are_stable() {
    let v = serde_json::to_value(ToSync::Command {
        id: 1,
        op: Op::SyncNow,
        args: Value::Null,
    })
    .unwrap();
    assert_eq!(v["type"], "command");
    assert_eq!(v["op"], "sync_now");
    let v = serde_json::to_value(ToSync::Settings(s3())).unwrap();
    assert_eq!(v["type"], "settings");
    assert_eq!(v["backend"]["type"], "s3");
    assert_eq!(Op::parse("export_key"), Some(Op::ExportKey));
    assert_eq!(Op::parse("create_key"), Some(Op::CreateKey));
    assert_eq!(Op::parse("nope"), None);
}

#[test]
fn empty_settings_use_defaults() {
    let d: SyncSettings = serde_json::from_value(json!({})).unwrap();
    assert_eq!(d, SyncSettings::default());
    assert_eq!(d.interval_min, 15);
}

#[test]
fn old_settings_with_scope_key_still_load() {
    let j: SyncSettings = serde_json::from_value(json!({
        "enabled": true,
        "scope": {"config": false, "schemas": true, "themes": true, "dicts": true}
    }))
    .unwrap();
    assert!(j.enabled);
    let t: SyncSettings = toml::from_str("enabled = true\n[scope]\nconfig = false\n").unwrap();
    assert!(t.enabled);
}

#[test]
fn skip_cert_verify_defaults_off_and_is_written_only_when_on() {
    let s = SyncSettings::default();
    assert!(!s.skip_cert_verify);
    assert!(!toml::to_string(&s).unwrap().contains("skip_cert_verify"));
    let on = SyncSettings {
        skip_cert_verify: true,
        ..Default::default()
    };
    let text = toml::to_string(&on).unwrap();
    assert!(text.contains("skip_cert_verify = true"), "{text}");
    let back: SyncSettings = toml::from_str(&text).unwrap();
    assert!(back.skip_cert_verify);
}

#[test]
fn validate_rejects_bad_settings() {
    assert!(SyncSettings::default().validate().is_ok());
    let mut s = SyncSettings {
        enabled: true,
        ..Default::default()
    };
    assert!(s.validate().is_err(), "启用须有后端");
    s.backend = Some(Backend::Webdav {
        url: "ftp://x".into(),
        user: "u".into(),
    });
    assert!(s.validate().is_err());
    s.backend = Some(Backend::Webdav {
        url: "https://x/dav".into(),
        user: "u".into(),
    });
    assert!(s.validate().is_ok());
    s.interval_min = 0;
    assert!(s.validate().is_err());
}

fn webdav(url: &str) -> SyncSettings {
    SyncSettings {
        backend: Some(Backend::Webdav {
            url: url.into(),
            user: "u".into(),
        }),
        ..Default::default()
    }
}

#[test]
fn validate_rejects_userinfo_in_urls() {
    for u in [
        "https://user:pass@dav.example.com/x",
        "https://user@dav.example.com/x",
        "http://a:b@127.0.0.1/x",
    ] {
        let e = webdav(u).validate().unwrap_err();
        assert!(e.contains("用户名或密码"), "{u}: {e}");
    }
    let mut s = webdav("https://dav.example.com/a@b");
    assert!(s.validate().is_ok(), "路径里的 @ 不是 userinfo");
    s.backend = Some(Backend::S3 {
        endpoint: "https://k:s@s3.example.com".into(),
        region: String::new(),
        bucket: "b".into(),
        prefix: String::new(),
        access_key_id: "AK".into(),
        path_style: false,
    });
    assert!(s.validate().is_err());
}

#[test]
fn validate_allows_plain_http_only_for_local_hosts() {
    for u in [
        "http://localhost:8080/dav",
        "http://127.0.0.1/x",
        "http://10.1.2.3/x",
        "http://172.16.0.9/x",
        "http://192.168.1.5:5005/x",
        "http://[::1]:80/x",
        "http://169.254.1.1/x",
    ] {
        assert!(webdav(u).validate().is_ok(), "{u}");
    }
    for u in [
        "http://dav.example.com/x",
        "http://8.8.8.8/x",
        "http://172.32.0.1/x",
        "http://192.169.0.1/x",
        "http://localhost.evil.com/x",
    ] {
        let e = webdav(u).validate().unwrap_err();
        assert!(e.contains("https"), "{u}: {e}");
    }
    assert!(webdav("https://dav.example.com/x").validate().is_ok());
}

#[test]
fn confirm_mass_delete_op_wire_name() {
    assert_eq!(
        Op::parse("confirm_mass_delete"),
        Some(Op::ConfirmMassDelete)
    );
}

#[test]
fn oversized_and_truncated_frames_error() {
    let mut huge = ((MAX_FRAME + 1) as u32).to_be_bytes().to_vec();
    huge.extend_from_slice(b"{}");
    assert!(read_frame::<_, ToSync>(&mut huge.as_slice()).is_err());
    let mut buf = Vec::new();
    write_frame(&mut buf, &ToSync::Shutdown).unwrap();
    buf.truncate(buf.len() - 1);
    assert!(read_frame::<_, ToSync>(&mut buf.as_slice()).is_err());
    assert!(
        read_frame::<_, ToSync>(&mut [].as_slice())
            .unwrap()
            .is_none()
    );
}

#[test]
fn device_ops_wire_names() {
    assert_eq!(Op::parse("list_devices"), Some(Op::ListDevices));
    assert_eq!(Op::parse("remove_device"), Some(Op::RemoveDevice));
}

#[test]
fn device_name_defaults_empty_and_is_length_limited() {
    let d: SyncSettings = serde_json::from_value(json!({})).unwrap();
    assert_eq!(d.device_name, "");
    let mut s = SyncSettings {
        device_name: "办".repeat(MAX_DEVICE_NAME_CHARS),
        ..Default::default()
    };
    assert!(s.validate().is_ok(), "按字符计数，64 个汉字可以");
    s.device_name.push('x');
    assert!(s.validate().is_err());
}

#[test]
fn group_ops_wire_names() {
    for (name, op) in [
        ("rename_device", Op::RenameDevice),
        ("set_group_name", Op::SetGroupName),
        ("join_group", Op::JoinGroup),
        ("leave_group", Op::LeaveGroup),
        ("claim_device", Op::ClaimDevice),
        ("confirm_identity", Op::ConfirmIdentity),
        ("purge_device", Op::PurgeDevice),
    ] {
        assert_eq!(Op::parse(name), Some(op), "{name}");
        assert_eq!(serde_json::to_value(op).unwrap(), json!(name));
    }
}

#[test]
fn hello_without_portable_decodes_as_false() {
    let m: ToSync = serde_json::from_value(json!({
        "type": "hello", "suffix": "", "userdata_dir": "/u", "state_dir": "/s", "settings": {},
    }))
    .unwrap();
    assert!(
        matches!(
            m,
            ToSync::Hello {
                portable: false,
                ..
            }
        ),
        "{m:?}"
    );
}

#[test]
fn status_carries_identity_fields() {
    let v = serde_json::to_value(Status {
        device: "pc".into(),
        group: "home".into(),
        group_name: "家".into(),
        needs_identity: true,
        ..Default::default()
    })
    .unwrap();
    assert_eq!(
        (
            &v["device"],
            &v["group"],
            &v["group_name"],
            &v["needs_identity"]
        ),
        (&json!("pc"), &json!("home"), &json!("家"), &json!(true))
    );
}

#[test]
fn status_without_round_fields_still_decodes() {
    let st: Status = serde_json::from_str(r#"{"phase":"idle"}"#).unwrap();
    assert_eq!(st.round, 0);
    assert_eq!(st.last_round, None);
    roundtrip(Status {
        round: 3,
        last_round: Some(RoundStats {
            ok: false,
            downloaded: 1,
            uploaded: 2,
            conflicts: 3,
            error: Some("x".into()),
            forced: true,
        }),
        ..Default::default()
    });
}

#[test]
fn content_defaults_all_on() {
    let s: SyncSettings = serde_json::from_str(r#"{"enabled":true}"#).unwrap();
    assert_eq!(s.content, SyncContent::default());
    assert!(s.content.learned && s.content.completion && s.content.stats);
    let s: SyncSettings = serde_json::from_str(r#"{"content":{"stats":false}}"#).unwrap();
    assert!(s.content.learned && s.content.completion && !s.content.stats);
}

#[test]
fn content_all_on_is_omitted_when_serialized() {
    let v = serde_json::to_value(SyncSettings::default()).unwrap();
    assert!(v.get("content").is_none());
    let s = SyncSettings {
        content: SyncContent {
            learned: false,
            ..Default::default()
        },
        ..Default::default()
    };
    let v = serde_json::to_value(&s).unwrap();
    assert_eq!(v["content"]["learned"], false);
    assert_eq!(serde_json::from_value::<SyncSettings>(v).unwrap(), s);
}

#[test]
fn status_omits_empty_removed_with_data() {
    let v = serde_json::to_value(Status::default()).unwrap();
    assert!(v.get("removed_with_data").is_none(), "{v}");
    let s = Status {
        removed_with_data: vec!["pc".into()],
        ..Default::default()
    };
    let v = serde_json::to_value(&s).unwrap();
    assert_eq!(v["removed_with_data"], json!(["pc"]));
    assert_eq!(serde_json::from_value::<Status>(v).unwrap(), s);
}

#[test]
fn status_carries_usage() {
    let v = serde_json::to_value(Status::default()).unwrap();
    for k in ["usage", "usage_busy", "usage_error"] {
        assert!(v.get(k).is_none(), "空时省略 {k}: {v}");
    }
    let s = Status {
        usage: Some(Usage {
            computed_at: 1_795_000_000,
            total: 3000,
            blobs: 2000,
            blobs_unreferenced: Some(100),
            devices: vec![DeviceUsage {
                id: "pc".into(),
                learned: 700,
                stats: 200,
            }],
            other: 100,
            provider: Some(ProviderUsage {
                used: 5000,
                available: Some(10),
                quota: None,
            }),
        }),
        usage_busy: true,
        usage_error: Some("断网".into()),
        ..Default::default()
    };
    roundtrip(FromSync::Status(s.clone()));
    let v = serde_json::to_value(&s).unwrap();
    assert_eq!(v["usage"]["devices"][0]["stats"], 200);
    assert_eq!(v["usage"]["provider"]["used"], 5000);
    assert_eq!(v["usage_busy"], true);
    let unknown = Usage::default();
    let v = serde_json::to_value(&unknown).unwrap();
    assert!(v.get("blobs_unreferenced").is_none(), "无法确定时省略: {v}");
    assert_eq!(Op::parse("refresh_usage"), Some(Op::RefreshUsage));
}
