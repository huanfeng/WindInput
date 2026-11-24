use super::*;

#[test]
fn record_roundtrips_and_crc_catches_corruption() {
    let r = Record {
        seq: 7,
        ts_ms: 1_700_000_000_000,
        kind: OpKind::Upsert,
        target: Target::Temp,
        schema: "pinyin".into(),
        code: "nihao".into(),
        text: "你好".into(),
        payload: Payload::Weight(10),
        boundary: 0b101,
    };
    let bytes = r.encode();
    assert_eq!(Record::decode(&bytes).unwrap(), r);
    assert_eq!(Record::frame_len(&bytes), Some(bytes.len()));
    let mut bad = bytes.clone();
    let last = bad.len() - 1;
    bad[last] ^= 0xff;
    assert!(Record::decode(&bad).is_err(), "CRC 必须抓到篡改");
    let short = &bytes[..bytes.len() - 3];
    assert!(
        Record::decode(short).is_err(),
        "半条记录必须报错而不是 panic"
    );
    assert!(Record::decode(&[]).is_err());
}

#[test]
fn every_payload_variant_roundtrips() {
    for payload in [
        Payload::None,
        Payload::Weight(-5),
        Payload::Count(u32::MAX),
        Payload::Ts(-1),
        Payload::Kind(3),
    ] {
        let r = Record {
            seq: 1,
            ts_ms: 0,
            kind: OpKind::BatchMarker,
            target: Target::Shadow,
            schema: String::new(),
            code: String::new(),
            text: "多行\n文本\t制表".into(),
            payload,
            boundary: 0,
        };
        assert_eq!(Record::decode(&r.encode()).unwrap(), r);
    }
}
