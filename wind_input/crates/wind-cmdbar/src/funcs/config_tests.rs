use super::*;
use crate::context::MemoryContext;
use crate::services::Services;
use std::sync::Mutex;

/// 记录 set / set_quiet 的入参，其余操作返回失败（本组用例不走到）。
#[derive(Default)]
struct RecConfig {
    calls: Mutex<Vec<(String, String, bool)>>,
}
impl crate::services::ConfigService for RecConfig {
    fn get(&self, _key: &str) -> anyhow::Result<String> {
        unreachable!()
    }
    fn set(&self, key: &str, value: &str) -> anyhow::Result<()> {
        self.record(key, value, false)
    }
    fn set_quiet(&self, key: &str, value: &str, quiet: bool) -> anyhow::Result<()> {
        self.record(key, value, quiet)
    }
    fn toggle(&self, _key: &str) -> anyhow::Result<String> {
        unreachable!()
    }
}
impl RecConfig {
    fn record(&self, key: &str, value: &str, quiet: bool) -> anyhow::Result<()> {
        self.calls
            .lock()
            .unwrap()
            .push((key.to_string(), value.to_string(), quiet));
        Ok(())
    }
}

fn ctx_with(rec: &std::sync::Arc<RecConfig>) -> MemoryContext {
    let mut svc = Services::new();
    svc.config = Some(rec.clone());
    MemoryContext::new().with_services(svc)
}

#[test]
fn set_defaults_to_loud() {
    let rec = std::sync::Arc::new(RecConfig::default());
    let ctx = ctx_with(&rec);
    fn_set_named(&ctx, &["ui.theme.style".into(), "dark".into()], &[]).unwrap();
    assert_eq!(
        rec.calls.lock().unwrap().as_slice(),
        &[("ui.theme.style".into(), "dark".into(), false)]
    );
}

#[test]
fn set_toast_off_requests_quiet() {
    let rec = std::sync::Arc::new(RecConfig::default());
    let ctx = ctx_with(&rec);
    fn_set_named(
        &ctx,
        &["ui.theme.style".into(), "dark".into()],
        &[("toast".into(), "off".into())],
    )
    .unwrap();
    assert_eq!(
        rec.calls.lock().unwrap().as_slice(),
        &[("ui.theme.style".into(), "dark".into(), true)]
    );
}

#[test]
fn set_rejects_unknown_toast_mode() {
    let rec = std::sync::Arc::new(RecConfig::default());
    let ctx = ctx_with(&rec);
    let err = fn_set_named(
        &ctx,
        &["ui.theme.style".into(), "dark".into()],
        &[("toast".into(), "error".into())],
    )
    .unwrap_err();
    assert!(err.to_string().contains("toast"), "实际: {err}");
    assert!(rec.calls.lock().unwrap().is_empty(), "校验失败不得写回");
}

/// 具名参数白名单写进 spec，写错的参数名在求值期就该报错（eval 层守卫），
/// 这里只钉 spec 声明本身。
#[test]
fn set_declares_toast_named_param() {
    let spec = specs()
        .into_iter()
        .find(|s| s.name == "config.set")
        .unwrap();
    assert!(spec.accepts_named("toast"));
    assert!(!spec.accepts_named("ok"));
    assert!(spec.eval_named.is_some());
}

/// 端到端：词条里写 `config.set(…, toast="off")` 要能解析、求值并把 quiet 传到服务。
#[test]
fn phrase_syntax_toast_off_reaches_service() {
    use crate::phrase;
    let rec = std::sync::Arc::new(RecConfig::default());
    let ctx = ctx_with(&rec);
    let reg = crate::registry::Registry::full();
    let r = phrase::evaluate_phrase(
        r#"$CC("[开关]", config.set("ui.theme.style", "dark", toast="off"))"#,
        &ctx,
        &reg,
    )
    .unwrap();
    match r {
        phrase::PhraseEval::Single {
            actions: a,
            on_error,
            ..
        } => {
            phrase::run_actions(&a, &ctx, &reg, on_error);
        }
        _ => panic!(),
    }
    assert_eq!(
        rec.calls.lock().unwrap().as_slice(),
        &[("ui.theme.style".into(), "dark".into(), true)]
    );
}

/// 默认（不带 toast）走完整管线同样通，语义保持改动前。
#[test]
fn phrase_syntax_without_toast_still_loud() {
    use crate::phrase;
    let rec = std::sync::Arc::new(RecConfig::default());
    let ctx = ctx_with(&rec);
    let reg = crate::registry::Registry::full();
    let r = phrase::evaluate_phrase(
        r#"$CC("[开关]", config.set("ui.theme.style", "dark"))"#,
        &ctx,
        &reg,
    )
    .unwrap();
    match r {
        phrase::PhraseEval::Single {
            actions: a,
            on_error,
            ..
        } => {
            phrase::run_actions(&a, &ctx, &reg, on_error);
        }
        _ => panic!(),
    }
    assert_eq!(
        rec.calls.lock().unwrap().as_slice(),
        &[("ui.theme.style".into(), "dark".into(), false)]
    );
}
