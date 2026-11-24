use super::*;

fn py(code: &str, boundary: u64) -> Candidate {
    Candidate {
        text: "你好".into(),
        code: code.into(),
        boundary,
        source: CandidateSource::Pinyin,
        ..Default::default()
    }
}

/// ni|hao：bit0 + bit2。
const NI_HAO: u64 = 0b101;

#[test]
fn picks_syllable_by_boundary() {
    let c = py("nihao", NI_HAO);
    assert_eq!(
        Coordinator::select_char_freq_code(&c, 2, 0).as_deref(),
        Some("ni")
    );
    assert_eq!(
        Coordinator::select_char_freq_code(&c, 2, 1).as_deref(),
        Some("hao")
    );
}

/// 音节数与字数对不上（儿化等）/ 没有边界 / 非拼音来源：拿不到可靠编码，不记词频。
#[test]
fn unreliable_code_yields_none() {
    let c = py("nihao", NI_HAO);
    assert_eq!(
        Coordinator::select_char_freq_code(&c, 3, 1),
        None,
        "音节数 ≠ 字数"
    );
    assert_eq!(
        Coordinator::select_char_freq_code(&py("nihao", 0), 2, 1),
        None,
        "无边界不猜切分"
    );
    let ct = Candidate {
        source: CandidateSource::CodeTable,
        ..py("wqvb", 0b1)
    };
    assert_eq!(
        Coordinator::select_char_freq_code(&ct, 2, 1),
        None,
        "码表不记"
    );
}
