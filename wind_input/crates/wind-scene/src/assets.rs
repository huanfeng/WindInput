//! 内存资源源：主题图片的相对引用（如 `assets/panel.png`）先在调用方登记的资源表里找，找不到
//! 再走原有的 `data:` / 文件路径（见 [`crate::theme_assets::asset_path`]）。
//!
//! 给没有文件系统的调用方用（主题编辑器的 wasm 预览：主题包的图片在内存里）。
//!
//! 资源表由调用方持有，连同一个**代号**（[`next_generation`]，进程内唯一）。排版 / 绘制前
//! 用 [`install`] 把它登记成本线程的当前表（只是克隆一个 `Arc`）；没登记时一切照旧。多个调用方
//! 共用一个线程（同页两个预览）时各自在用之前登记自己那份，互不顶替。
//!
//! 命中的引用被改写成 `mem:<代号>/<规范化路径>` 交给图片缓存：图片缓存按源串缓存解码结果，
//! 代号不同的两份表即使路径同名也是不同的源。

use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

/// 内存资源源串的前缀。
const SCHEME: &str = "mem:";

/// 相对路径 → 字节。
#[derive(Clone, Debug, Default)]
pub struct MemoryAssets {
    files: HashMap<String, Arc<[u8]>>,
}

impl MemoryAssets {
    pub fn new() -> Self {
        Self::default()
    }

    /// 登记一个文件；路径按 [`normalize`] 规范化，同名后者覆盖前者。规范化不出来的路径
    /// （空、`..` 越过根）不登记，返回 false。
    pub fn insert(&mut self, path: &str, bytes: impl Into<Arc<[u8]>>) -> bool {
        match normalize(path) {
            Some(p) => {
                self.files.insert(p, bytes.into());
                true
            }
            None => false,
        }
    }

    pub fn get(&self, path: &str) -> Option<&Arc<[u8]>> {
        self.files.get(&normalize(path)?)
    }

    pub fn len(&self) -> usize {
        self.files.len()
    }

    pub fn is_empty(&self) -> bool {
        self.files.is_empty()
    }
}

/// 相对路径规范化：反斜杠换成 `/`，去掉空段与 `.`，`..` 退一级；退过根（主题目录之外）
/// 或什么都不剩返回 None。
fn normalize(path: &str) -> Option<String> {
    let p = path.replace('\\', "/");
    let mut out: Vec<&str> = Vec::new();
    for seg in p.split('/') {
        match seg {
            "" | "." => {}
            ".." => {
                out.pop()?;
            }
            s => out.push(s),
        }
    }
    (!out.is_empty()).then(|| out.join("/"))
}

static NEXT_GEN: AtomicU64 = AtomicU64::new(1);

/// 新代号（进程内唯一）。资源表内容变了就换一个，图片缓存里旧代号的源串不再被引用。
pub fn next_generation() -> u64 {
    NEXT_GEN.fetch_add(1, Ordering::Relaxed)
}

thread_local! {
    /// 当前登记的资源表与它的代号。
    static CURRENT: RefCell<Option<(u64, Arc<MemoryAssets>)>> = const { RefCell::new(None) };
}

/// 把 `assets`（代号 `generation`）登记成本线程的当前表，替换上一份。
pub fn install(generation: u64, assets: &Arc<MemoryAssets>) {
    CURRENT.with(|c| *c.borrow_mut() = Some((generation, Arc::clone(assets))));
}

/// 撤掉本线程的资源表。
pub fn uninstall() {
    CURRENT.with(|c| *c.borrow_mut() = None);
}

/// 相对引用在当前资源表里时，返回交给图片缓存的源串；否则 None（含没登记、绝对路径、`data:`）。
pub fn source_key(reference: &str) -> Option<String> {
    if reference.starts_with("data:") || std::path::Path::new(reference).is_absolute() {
        return None;
    }
    CURRENT.with(|c| {
        let c = c.borrow();
        let (generation, assets) = c.as_ref()?;
        let key = normalize(reference)?;
        assets
            .files
            .contains_key(&key)
            .then(|| format!("{SCHEME}{generation}/{key}"))
    })
}

/// 图片源串是内存资源时取字节：`None` = 不是内存资源（按原路径读）；`Some(None)` = 是，但
/// 不在当前登记的那份表里（别的代号）。
pub(crate) fn bytes_of(src: &str) -> Option<Option<Arc<[u8]>>> {
    let rest = src.strip_prefix(SCHEME)?;
    Some(CURRENT.with(|c| {
        let c = c.borrow();
        let (generation, assets) = c.as_ref()?;
        let (g, path) = rest.split_once('/')?;
        if g.parse::<u64>().ok()? != *generation {
            return None;
        }
        assets.files.get(path).cloned()
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bytes(b: &[u8]) -> Arc<[u8]> {
        Arc::from(b)
    }

    fn table(path: &str, b: &[u8]) -> Arc<MemoryAssets> {
        let mut a = MemoryAssets::new();
        assert!(a.insert(path, bytes(b)));
        Arc::new(a)
    }

    #[test]
    fn paths_normalize_on_insert_and_lookup() {
        let mut a = MemoryAssets::new();
        a.insert(".\\assets\\a.png", bytes(b"x"));
        assert_eq!(a.get("assets/a.png").map(|b| &b[..]), Some(&b"x"[..]));
        assert_eq!(a.get("./assets//a.png").map(|b| &b[..]), Some(&b"x"[..]));
        assert_eq!(
            a.get("img/../assets/./a.png").map(|b| &b[..]),
            Some(&b"x"[..])
        );
        assert!(a.get("assets/b.png").is_none());
        assert!(a.get("../assets/a.png").is_none(), "越过根查不到");
        assert!(!a.insert("../x.png", bytes(b"y")), "越过根不登记");
        assert!(!a.insert("./", bytes(b"y")));
        assert_eq!(a.len(), 1);
    }

    #[test]
    fn source_key_only_for_registered_relative_refs() {
        uninstall();
        assert_eq!(source_key("a.png"), None, "没登记时一切照旧");
        let g = next_generation();
        install(g, &table("a.png", b"1"));
        let k = source_key("./a.png").expect("登记过的相对引用");
        assert_eq!(k, format!("{SCHEME}{g}/a.png"));
        assert_eq!(bytes_of(&k).flatten().as_deref(), Some(&b"1"[..]));
        assert_eq!(source_key("b.png"), None, "表里没有的走原路径");
        assert_eq!(source_key("../a.png"), None);
        assert_eq!(source_key("data:image/png;base64,AA=="), None);
        assert_eq!(bytes_of("/abs/a.png"), None, "不是内存源串");
        uninstall();
        assert_eq!(bytes_of(&k), Some(None));
    }

    /// 两个调用方各持一份表、各自用前登记：源串按代号区分，轮流登记后都还读得到自己的图。
    #[test]
    fn two_owners_take_turns_without_clobbering() {
        let (ga, a) = (next_generation(), table("a.png", b"A"));
        let (gb, b) = (next_generation(), table("a.png", b"B"));
        install(ga, &a);
        let ka = source_key("a.png").unwrap();
        install(gb, &b);
        let kb = source_key("a.png").unwrap();
        assert_ne!(ka, kb);
        assert_eq!(bytes_of(&kb).flatten().as_deref(), Some(&b"B"[..]));
        install(ga, &a);
        assert_eq!(bytes_of(&ka).flatten().as_deref(), Some(&b"A"[..]));
        uninstall();
    }
}
