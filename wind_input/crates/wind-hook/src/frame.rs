//! 帧：4 字节大端长度 + JSON 对象。帧层在消息上加 `v`（协议版本）、`seq`（每条管道单调递增）、
//! `dropped`（上一条成功投递以来丢了几条，0 时省略）。

use std::io::{self, Read, Write};

use serde_json::Value;

use crate::PROTOCOL_VERSION;
use crate::msg::Msg;

/// 单帧上限。事件都很小，超过即视为对端出错，断开。
pub const MAX_FRAME: usize = 1024 * 1024;

#[derive(Debug, Clone, PartialEq)]
pub struct Frame {
    pub seq: u64,
    pub dropped: u64,
    pub msg: Msg,
}

impl Frame {
    pub fn new(msg: Msg) -> Frame {
        Frame {
            seq: 0,
            dropped: 0,
            msg,
        }
    }
}

/// 编码成线上字节（含长度前缀）。
pub fn encode(frame: &Frame) -> Vec<u8> {
    let mut obj = match serde_json::to_value(&frame.msg) {
        Ok(Value::Object(o)) => o,
        // Msg 是内部标签枚举，序列化结果恒为对象。
        _ => serde_json::Map::new(),
    };
    obj.insert("v".into(), PROTOCOL_VERSION.into());
    if frame.seq != 0 {
        obj.insert("seq".into(), frame.seq.into());
    }
    if frame.dropped != 0 {
        obj.insert("dropped".into(), frame.dropped.into());
    }
    let payload = serde_json::to_vec(&Value::Object(obj)).unwrap_or_default();
    let mut buf = Vec::with_capacity(4 + payload.len());
    buf.extend_from_slice(&(payload.len() as u32).to_be_bytes());
    buf.extend_from_slice(&payload);
    buf
}

/// 从载荷（不含长度前缀）解码。
pub fn decode(payload: &[u8]) -> io::Result<Frame> {
    let v: Value = serde_json::from_slice(payload).map_err(invalid)?;
    let num = |k: &str| v.get(k).and_then(Value::as_u64).unwrap_or(0);
    let (seq, dropped) = (num("seq"), num("dropped"));
    let msg = serde_json::from_value(v).map_err(invalid)?;
    Ok(Frame { seq, dropped, msg })
}

pub fn write_frame<W: Write>(w: &mut W, frame: &Frame) -> io::Result<()> {
    w.write_all(&encode(frame))?;
    w.flush()
}

/// 读一帧。对端正常关闭（帧边界处 EOF）返回 `Ok(None)`。
pub fn read_frame<R: Read>(r: &mut R) -> io::Result<Option<Frame>> {
    let mut len = [0u8; 4];
    match r.read_exact(&mut len) {
        Ok(()) => {}
        Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => return Ok(None),
        Err(e) => return Err(e),
    }
    let len = u32::from_be_bytes(len) as usize;
    if len > MAX_FRAME {
        return Err(invalid(format!("帧长 {len} 超过上限")));
    }
    let mut buf = vec![0u8; len];
    r.read_exact(&mut buf)?;
    decode(&buf).map(Some)
}

fn invalid(e: impl std::fmt::Display) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, e.to_string())
}

#[cfg(test)]
mod tests {
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
}
