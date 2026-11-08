//! 内存资源源：主题图片的相对引用（如 `assets/panel.png`）先在调用方登记的资源表里找，找不到
//! 再走原有的 `data:` / 文件路径（见 [`crate::theme_assets::asset_path`]）。
//!
//! 给没有文件系统的调用方用（主题编辑器的 wasm 预览：主题包的图片在内存里）。登记的表是
//! **线程局部**的，与图片缓存同一个线程（UI 单线程）；没登记时一切照旧。
//!
//! 命中的引用被改写成 `mem:<代号>/<规范化路径>` 交给图片缓存：图片缓存按源串缓存解码结果，
//! 代号随每次 [`install`] 递增，换一套资源表后同名路径不会命中上一套的旧图。

use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::Arc;

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

    /// 登记一个文件；路径按 [`normalize`] 规范化，同名后者覆盖前者。
    pub fn insert(&mut self, path: &str, bytes: impl Into<Arc<[u8]>>) {
        self.files.insert(normalize(path), bytes.into());
    }

    pub fn get(&self, path: &str) -> Option<&Arc<[u8]>> {
        self.files.get(&normalize(path))
    }

    pub fn len(&self) -> usize {
        self.files.len()
    }

    pub fn is_empty(&self) -> bool {
        self.files.is_empty()
    }
}

/// 相对路径规范化：反斜杠换成 `/`，去掉开头的 `./` 与 `/`。
fn normalize(path: &str) -> String {
    let p = path.replace('\\', "/");
    let mut s = p.as_str();
    loop {
        if let Some(r) = s.strip_prefix("./") {
            s = r;
        } else if let Some(r) = s.strip_prefix('/') {
            s = r;
        } else {
            break;
        }
    }
    s.to_string()
}

thread_local! {
    /// 当前登记的资源表与它的代号。
    static CURRENT: RefCell<Option<(u64, MemoryAssets)>> = const { RefCell::new(None) };
    /// 下一个代号。
    static NEXT_GEN: std::cell::Cell<u64> = const { std::cell::Cell::new(1) };
}

/// 登记本线程的资源表（替换上一套）。
pub fn install(assets: MemoryAssets) {
    let generation = NEXT_GEN.with(|g| {
        let v = g.get();
        g.set(v + 1);
        v
    });
    CURRENT.with(|c| *c.borrow_mut() = Some((generation, assets)));
}

/// 撤掉本线程的资源表。
pub fn uninstall() {
    CURRENT.with(|c| *c.borrow_mut() = None);
}

/// 相对引用在资源表里时，返回交给图片缓存的源串；否则 None（含没登记、绝对路径、`data:`）。
pub fn source_key(reference: &str) -> Option<String> {
    if reference.starts_with("data:") || std::path::Path::new(reference).is_absolute() {
        return None;
    }
    CURRENT.with(|c| {
        let c = c.borrow();
        let (generation, assets) = c.as_ref()?;
        let key = normalize(reference);
        assets
            .files
            .contains_key(&key)
            .then(|| format!("{SCHEME}{generation}/{key}"))
    })
}

/// 图片源串是内存资源时取字节：`None` = 不是内存资源（按原路径读）；`Some(None)` = 是，但
/// 已不在当前资源表里（换过表）。
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

    #[test]
    fn paths_normalize_on_insert_and_lookup() {
        let mut a = MemoryAssets::new();
        a.insert(".\\assets\\a.png", bytes(b"x"));
        assert_eq!(a.get("assets/a.png").map(|b| &b[..]), Some(&b"x"[..]));
        assert_eq!(a.get("./assets/a.png").map(|b| &b[..]), Some(&b"x"[..]));
        assert!(a.get("assets/b.png").is_none());
    }

    #[test]
    fn source_key_only_for_registered_relative_refs() {
        uninstall();
        assert_eq!(source_key("a.png"), None, "没登记时一切照旧");
        let mut a = MemoryAssets::new();
        a.insert("a.png", bytes(b"1"));
        install(a);
        let k = source_key("./a.png").expect("登记过的相对引用");
        assert!(k.starts_with(SCHEME), "{k}");
        assert_eq!(bytes_of(&k).flatten().as_deref(), Some(&b"1"[..]));
        assert_eq!(source_key("b.png"), None, "表里没有的走原路径");
        assert_eq!(source_key("data:image/png;base64,AA=="), None);
        assert_eq!(bytes_of("/abs/a.png"), None, "不是内存源串");
        uninstall();
    }

    /// 换一套表后旧源串失效：图片缓存按源串缓存，同名路径不能命中上一套的图。
    #[test]
    fn reinstall_changes_generation() {
        let mut a = MemoryAssets::new();
        a.insert("a.png", bytes(b"1"));
        install(a.clone());
        let k1 = source_key("a.png").unwrap();
        a.insert("a.png", bytes(b"2"));
        install(a);
        let k2 = source_key("a.png").unwrap();
        assert_ne!(k1, k2);
        assert_eq!(bytes_of(&k1), Some(None), "旧代号读不到");
        assert_eq!(bytes_of(&k2).flatten().as_deref(), Some(&b"2"[..]));
        uninstall();
        assert_eq!(bytes_of(&k2), Some(None));
    }
}
