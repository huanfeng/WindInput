//! 日志记录（设计 §3.2）：`len u32 | crc32 u32 | body`，body = seq、ts_ms、kind、target、
//! boundary u64、schema / code / text（各 len u16 + bytes）、payload（tag u8 + 值）。crc 覆盖 body。
//!
//! `boundary`（音节边界位图，与 wdat 条目同义）记进日志是为了回放时能给增量表建简拼分组
//! 索引——拍平后的码推不回边界，而线性扫 delta 算简拼不可用（设计 §3.3）。

use anyhow::{Context, bail};

/// 操作落到哪一类数据（设计 §3.2 target）。Delete / SetWeight 必须带目标，不做「所有文件都删」。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[repr(u8)]
pub enum Target {
    User = 0,
    Temp = 1,
    Draft = 2,
    Freq = 3,
    Assoc = 4,
    Phrases = 5,
    Shadow = 6,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum OpKind {
    Upsert = 0,
    Delete = 1,
    SetWeight = 2,
    Incr = 3,
    Touch = 4,
    Promote = 5,
    Expire = 6,
    BatchMarker = 7,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Payload {
    None,
    Weight(i32),
    Count(u32),
    Ts(i64),
    Kind(u8),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Record {
    pub seq: u64,
    pub ts_ms: i64,
    pub kind: OpKind,
    pub target: Target,
    pub schema: String,
    pub code: String,
    pub text: String,
    pub payload: Payload,
    /// 音节边界位图（0 = 无边界信息）。
    pub boundary: u64,
}

fn put_str(out: &mut Vec<u8>, s: &str) {
    out.extend_from_slice(&(s.len() as u16).to_le_bytes());
    out.extend_from_slice(s.as_bytes());
}

fn get_str<'a>(b: &'a [u8], pos: &mut usize) -> anyhow::Result<&'a str> {
    let n = u16::from_le_bytes(b.get(*pos..*pos + 2).context("短")?.try_into()?) as usize;
    *pos += 2;
    let s = std::str::from_utf8(b.get(*pos..*pos + n).context("短")?)?;
    *pos += n;
    Ok(s)
}

fn get<const N: usize>(b: &[u8], pos: &mut usize) -> anyhow::Result<[u8; N]> {
    let a: [u8; N] = b.get(*pos..*pos + N).context("短")?.try_into()?;
    *pos += N;
    Ok(a)
}

impl Target {
    fn from_u8(t: u8) -> anyhow::Result<Target> {
        Ok(match t {
            0 => Target::User,
            1 => Target::Temp,
            2 => Target::Draft,
            3 => Target::Freq,
            4 => Target::Assoc,
            5 => Target::Phrases,
            6 => Target::Shadow,
            t => bail!("未知 target {t}"),
        })
    }
}

impl OpKind {
    fn from_u8(k: u8) -> anyhow::Result<OpKind> {
        Ok(match k {
            0 => OpKind::Upsert,
            1 => OpKind::Delete,
            2 => OpKind::SetWeight,
            3 => OpKind::Incr,
            4 => OpKind::Touch,
            5 => OpKind::Promote,
            6 => OpKind::Expire,
            7 => OpKind::BatchMarker,
            k => bail!("未知 kind {k}"),
        })
    }
}

impl Record {
    /// 字符串字段按 `len u16` 编码：超长的必须在写盘前拒掉——截断长度会写出 CRC 合法却解不开
    /// 的帧，下次 open 从它截断，之后的记录全丢。
    pub fn check_lens(&self) -> anyhow::Result<()> {
        for (name, s) in [
            ("schema", &self.schema),
            ("code", &self.code),
            ("text", &self.text),
        ] {
            if s.len() > u16::MAX as usize {
                bail!("{name} 超长: {} 字节（上限 {}）", s.len(), u16::MAX);
            }
        }
        Ok(())
    }

    /// 帧：`len u32 | crc32 u32 | body`；`len` 是 body 长度，crc 覆盖 body。
    pub fn encode(&self) -> Vec<u8> {
        let mut body = Vec::with_capacity(64);
        body.extend_from_slice(&self.seq.to_le_bytes());
        body.extend_from_slice(&self.ts_ms.to_le_bytes());
        body.push(self.kind as u8);
        body.push(self.target as u8);
        body.extend_from_slice(&self.boundary.to_le_bytes());
        put_str(&mut body, &self.schema);
        put_str(&mut body, &self.code);
        put_str(&mut body, &self.text);
        match &self.payload {
            Payload::None => body.push(0),
            Payload::Weight(w) => {
                body.push(1);
                body.extend_from_slice(&w.to_le_bytes());
            }
            Payload::Count(c) => {
                body.push(2);
                body.extend_from_slice(&c.to_le_bytes());
            }
            Payload::Ts(t) => {
                body.push(3);
                body.extend_from_slice(&t.to_le_bytes());
            }
            Payload::Kind(k) => {
                body.push(4);
                body.push(*k);
            }
        }
        let mut out = Vec::with_capacity(body.len() + 8);
        out.extend_from_slice(&(body.len() as u32).to_le_bytes());
        out.extend_from_slice(&crc32fast::hash(&body).to_le_bytes());
        out.extend_from_slice(&body);
        out
    }

    /// 只解一条；`bytes` 多出来的尾巴忽略。帧不完整 / CRC 不符 ⇒ Err（绝不 panic）。
    pub fn decode(bytes: &[u8]) -> anyhow::Result<Record> {
        let len = u32::from_le_bytes(bytes.get(0..4).context("短")?.try_into()?) as usize;
        let crc = u32::from_le_bytes(bytes.get(4..8).context("短")?.try_into()?);
        let body = bytes.get(8..8 + len).context("帧不完整")?;
        if crc32fast::hash(body) != crc {
            bail!("crc 不符");
        }
        let mut p = 0usize;
        let seq = u64::from_le_bytes(get::<8>(body, &mut p)?);
        let ts_ms = i64::from_le_bytes(get::<8>(body, &mut p)?);
        let kind = OpKind::from_u8(get::<1>(body, &mut p)?[0])?;
        let target = Target::from_u8(get::<1>(body, &mut p)?[0])?;
        let boundary = u64::from_le_bytes(get::<8>(body, &mut p)?);
        let schema = get_str(body, &mut p)?.to_string();
        let code = get_str(body, &mut p)?.to_string();
        let text = get_str(body, &mut p)?.to_string();
        let payload = match get::<1>(body, &mut p)?[0] {
            0 => Payload::None,
            1 => Payload::Weight(i32::from_le_bytes(get::<4>(body, &mut p)?)),
            2 => Payload::Count(u32::from_le_bytes(get::<4>(body, &mut p)?)),
            3 => Payload::Ts(i64::from_le_bytes(get::<8>(body, &mut p)?)),
            4 => Payload::Kind(get::<1>(body, &mut p)?[0]),
            t => bail!("未知 payload {t}"),
        };
        Ok(Record {
            seq,
            ts_ms,
            kind,
            target,
            schema,
            code,
            text,
            payload,
            boundary,
        })
    }

    /// 整帧长度（含 8 字节帧头），供日志顺序读取推进游标；头都不完整 ⇒ None。
    pub fn frame_len(bytes: &[u8]) -> Option<usize> {
        let len = u32::from_le_bytes(bytes.get(0..4)?.try_into().ok()?) as usize;
        Some(8 + len)
    }
}

#[cfg(test)]
#[path = "record_tests.rs"]
mod tests;
