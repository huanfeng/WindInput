use super::*;

/// 逐次重扫的变量收尾：`from` 起第一个不在参数内联色 `$[SPEC]{BODY}` 里的 `}`。
/// 朴素写法（逐处现找），与配对表的查表实现对拍。
fn rescan_var_end(b: &[u8], from: usize) -> Option<usize> {
    let mut j = from;
    while j < b.len() {
        if b[j] == b'$' && b.get(j + 1) == Some(&b'[') {
            let spec = (j + 2..b.len().min(j + 2 + COLOR_SPEC_MAX + 1))
                .find(|&k| matches!(b[k], b']' | b'{' | b'}' | b'\n'));
            if let Some(k) = spec
                && b[k] == b']'
                && b.get(k + 1) == Some(&b'{')
                && let Some(body) = (k + 2..b.len()).find(|&m| b[m] == b'}')
            {
                j = body + 1;
                continue;
            }
        }
        if b[j] == b'}' {
            return Some(j);
        }
        j += 1;
    }
    None
}

/// 逐次重扫实现，留作对拍基准（配对规则必须逐字相同）。
fn rescan_group_end(b: &[u8], from: usize) -> Option<usize> {
    let mut i = from;
    let mut depth = 0usize;
    while i < b.len() {
        if b[i] == b'$' && i + 1 < b.len() && b[i + 1] == b'{' {
            i = rescan_var_end(b, i + 2)? + 1;
            continue;
        }
        match b[i] {
            b'{' => depth += 1,
            b'}' if depth == 0 => return Some(i),
            b'}' => depth -= 1,
            _ => {}
        }
        i += 1;
    }
    None
}

/// 配对表与旧的逐次重扫在随机模板上逐位置一致（凡旧解析会去找配对的 `{`）。
#[test]
fn pairs_match_rescan_reference() {
    let alphabet = b"${}[]a|";
    let mut seed = 0x2545_F491_4F6C_DD1Du64;
    let mut next = || {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        seed
    };
    for _ in 0..20_000 {
        let len = (next() % 24) as usize;
        let b: Vec<u8> = (0..len)
            .map(|_| alphabet[(next() % alphabet.len() as u64) as usize])
            .collect();
        let pairs = Pairs::new(&b);
        // 只比「解析器会问到的」`{`：扫到它时它是记号边界（不在某个 `${…}` 里）。
        let mut i = 0;
        while i < b.len() {
            if b[i] == b'$' && i + 1 < b.len() && b[i + 1] == b'{' {
                let want = rescan_var_end(&b, i + 2);
                match want {
                    Some(c) => {
                        assert_eq!(
                            pairs.var_end(i + 2, b.len()),
                            Some(c),
                            "{:?} @ {i}",
                            String::from_utf8_lossy(&b)
                        );
                        i = c + 1;
                        continue;
                    }
                    // 未闭合：之后一切按字面，配对表不再回答（全为 None）。
                    None => break,
                }
            }
            if b[i] == b'{' {
                assert_eq!(
                    pairs.group_end(i, b.len()),
                    rescan_group_end(&b, i + 1),
                    "{:?} @ {i}",
                    String::from_utf8_lossy(&b)
                );
            }
            i += 1;
        }
    }
}

fn eval(name: &str, _: Option<&str>) -> Option<String> {
    (name == "a").then(|| "A".to_string())
}

/// 深度 5000 的嵌套不打穿栈：超过上限的层按字面文字输出，上限以内照常求值。
#[test]
fn deep_nesting_is_bounded() {
    let n = 5000;
    for (open, close) in [("{", "}"), ("$[accent]{", "}")] {
        let tpl = format!("{}${{a}}{}", open.repeat(n), close.repeat(n));
        let out = render(&tpl, 0, eval);
        assert!(out.contains('A'), "变量照常求值");
        // 上限以内的层被解析吃掉，超出的层原样留在输出里。
        let literal_opens = out.matches('{').count();
        assert_eq!(literal_opens, n - MAX_DEPTH, "{open}");
    }
}

/// 20 万个未闭合的 `{`（以及 `${`）在毫秒级完成：配对是一次线性扫描。
#[test]
fn unclosed_braces_are_linear() {
    for tpl in [
        "{".repeat(200_000),
        "${".repeat(100_000),
        "{${a}".repeat(40_000),
    ] {
        let t = std::time::Instant::now();
        let _ = Template::parse(&tpl);
        // 平方级实测 37 秒；线性扫描在调试构建下也远低于这个宽松上限。
        assert!(
            t.elapsed() < std::time::Duration::from_secs(2),
            "{:?}",
            t.elapsed()
        );
    }
}
