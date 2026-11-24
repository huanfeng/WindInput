use super::*;
use crate::msg::ModeEvent;

fn mode() -> Msg {
    Msg::Mode(ModeEvent {
        ts: 9,
        chinese: true,
        full_width: false,
        schema: "wubi86".into(),
    })
}

#[test]
fn round_trip_keeps_seq_and_dropped() {
    let f = Frame {
        seq: 7,
        dropped: 2,
        msg: mode(),
    };
    let bytes = encode(&f);
    let got = read_frame(&mut bytes.as_slice()).unwrap().unwrap();
    assert_eq!(got, f);
}

#[test]
fn wire_carries_version_and_omits_zero_counters() {
    let bytes = encode(&Frame::new(Msg::Ping));
    let v: Value = serde_json::from_slice(&bytes[4..]).unwrap();
    assert_eq!(v["t"], "ping");
    assert_eq!(v["v"], PROTOCOL_VERSION);
    assert!(v.get("seq").is_none() && v.get("dropped").is_none());
}

#[test]
fn clean_eof_is_none_and_truncation_is_error() {
    assert!(read_frame(&mut &[][..]).unwrap().is_none());
    let bytes = encode(&Frame::new(mode()));
    assert!(read_frame(&mut &bytes[..bytes.len() - 1]).is_err());
}

#[test]
fn oversized_frame_is_rejected() {
    let len = ((MAX_FRAME + 1) as u32).to_be_bytes();
    assert!(read_frame(&mut &len[..]).is_err());
}

#[test]
fn consecutive_frames_read_in_order() {
    let mut bytes = encode(&Frame::new(Msg::Ping));
    bytes.extend(encode(&Frame::new(mode())));
    let mut r = bytes.as_slice();
    assert_eq!(read_frame(&mut r).unwrap().unwrap().msg, Msg::Ping);
    assert_eq!(read_frame(&mut r).unwrap().unwrap().msg, mode());
    assert!(read_frame(&mut r).unwrap().is_none());
}
