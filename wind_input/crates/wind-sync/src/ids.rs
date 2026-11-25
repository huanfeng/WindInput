//! 设备号与组号：短、可读、可手输，会拼进远端键。

use crate::crypto::random_bytes;

pub const MAX_ID_LEN: usize = 24;
/// 由名称生成时截到的长度。
const NAME_LEN: usize = 16;
/// 随机码长度。
const RANDOM_LEN: usize = 4;
/// 随机码字母表：去掉易混的 0/1/i/l/o。
const ALPHABET: &str = "23456789abcdefghjkmnpqrstuvwxyz";

/// `[a-z0-9-]`，长 1–24，不以 `-` 开头或结尾。
pub fn valid_id(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= MAX_ID_LEN
        && !s.starts_with('-')
        && !s.ends_with('-')
        && s.bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

/// 名称转号：小写、非法字符换 `-`、合并连续 `-`、去首尾 `-`、截到 16；结果为空返回 None。
pub fn id_from_name(name: &str) -> Option<String> {
    let mut out = String::new();
    for c in name.chars().map(|c| c.to_ascii_lowercase()) {
        let c = if c.is_ascii_lowercase() || c.is_ascii_digit() {
            c
        } else {
            '-'
        };
        if c != '-' || !out.ends_with('-') {
            out.push(c);
        }
    }
    out.truncate(NAME_LEN);
    let out = out.trim_matches('-');
    (!out.is_empty()).then(|| out.to_string())
}

pub fn random_id() -> String {
    let alphabet = ALPHABET.as_bytes();
    random_bytes::<RANDOM_LEN>()
        .iter()
        .map(|b| alphabet[*b as usize % alphabet.len()] as char)
        .collect()
}

/// 默认设备号：取自名称，取不出则随机；绿色版加 `-portable`。
pub fn default_device_id(name: &str, portable: bool) -> String {
    let base = id_from_name(name).unwrap_or_else(random_id);
    if portable {
        format!("{base}-portable")
    } else {
        base
    }
}

/// 重名时加序号：`n >= 2` 得 `<base>-<n>`，必要时截短 base 使总长不超限。
pub fn with_suffix(base: &str, n: u32) -> String {
    if n < 2 {
        return base.to_string();
    }
    let suffix = format!("-{n}");
    let keep = MAX_ID_LEN.saturating_sub(suffix.len());
    let head = &base[..base.len().min(keep)];
    format!("{}{suffix}", head.trim_end_matches('-'))
}

#[cfg(test)]
#[path = "ids_tests.rs"]
mod tests;
