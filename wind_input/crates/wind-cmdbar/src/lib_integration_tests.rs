use super::*;

/// 端到端：解析 → 求值 → 执行动作（mock 服务）。
#[test]
fn end_to_end_command_with_effect() {
    use std::sync::{Arc, Mutex};

    #[derive(Default)]
    struct Log(Mutex<Vec<String>>);
    impl KeyInjector for Log {
        fn tap(&self, c: &str) -> anyhow::Result<()> {
            self.0.lock().unwrap().push(format!("tap:{c}"));
            Ok(())
        }
        fn sequence(&self, _: &[String]) -> anyhow::Result<()> {
            Ok(())
        }
        fn hold(&self, _: &str) -> anyhow::Result<()> {
            Ok(())
        }
        fn release(&self, _: &str) -> anyhow::Result<()> {
            Ok(())
        }
        fn type_text(&self, _: &str) -> anyhow::Result<()> {
            Ok(())
        }
    }

    let log = Arc::new(Log::default());
    let mut svc = Services::new();
    svc.keys = Some(log.clone());
    let ctx = MemoryContext::new().with_services(svc);
    let reg = Registry::full();

    // $CC("《》", type("《》"), key.tap("Left"))：上屏《》并按 Left。
    let phrase = parse(r#"$CC("《》", type("《》"), key.tap("Left"))"#).unwrap();
    let ev = evaluate(&phrase, &ctx, &reg).unwrap();
    assert_eq!(ev.display, "《》");
    assert_eq!(ev.actions.len(), 2);

    // 执行：text 动作返回上屏文本，effect 动作触发副作用。
    let mut inserted = String::new();
    for act in &ev.actions {
        inserted.push_str(&act.run(&ctx, &reg).unwrap());
    }
    assert_eq!(inserted, "《》");
    assert_eq!(log.0.lock().unwrap().as_slice(), &["tap:Left".to_string()]);
}

#[test]
fn template_uses_context() {
    let reg = Registry::with_builtins();
    let ctx = MemoryContext::new().with_input("abc");
    let p = parse("编码 {code} 共 {len(code)} 位").unwrap();
    assert_eq!(
        evaluate(&p, &ctx, &reg).unwrap().display,
        "编码 abc 共 3 位"
    );
}
