//! 本地文件集抽象（用户目录里范围内的文件）。S4 的真实实现负责 `data.beginEdit`/原子写入/重载。

use crate::scope::{Namespace, to_local, to_remote};
use std::collections::BTreeMap;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalError(pub String);

pub trait LocalStore {
    /// 全部文件的相对路径（`/` 分隔）；范围过滤由引擎做。
    fn list(&self) -> Result<Vec<String>, LocalError>;
    fn read(&self, path: &str) -> Result<Option<Vec<u8>>, LocalError>;
    /// 一批写入作为一次编辑会话（真实实现在此前后 beginEdit/endEdit 并合并重载）。
    fn write_batch(&mut self, files: &[(String, Vec<u8>)]) -> Result<(), LocalError>;
    fn delete(&mut self, paths: &[String]) -> Result<(), LocalError>;
    /// 文件是否带可执行位（Windows 恒为 false）。
    fn is_exec(&self, _path: &str) -> Result<bool, LocalError> {
        Ok(false)
    }
    /// 给这些文件加上可执行位（保留其余权限位；Windows 忽略）。
    fn set_exec(&mut self, _paths: &[String]) -> Result<(), LocalError> {
        Ok(())
    }
}

/// 内存实现（测试与 S4 之前的联调）。
#[derive(Debug, Clone, Default)]
pub struct MemLocal {
    pub files: BTreeMap<String, Vec<u8>>,
    /// 累计 write_batch 调用次数（断言「一批写入合并成一次」）。
    pub batches: usize,
    /// 带可执行位的路径。
    pub exec: std::collections::BTreeSet<String>,
}

impl MemLocal {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn set(&mut self, path: &str, content: &str) {
        self.files.insert(path.into(), content.as_bytes().to_vec());
    }
    pub fn text(&self, path: &str) -> Option<String> {
        self.files
            .get(path)
            .map(|b| String::from_utf8_lossy(b).into_owned())
    }
}

impl LocalStore for MemLocal {
    fn list(&self) -> Result<Vec<String>, LocalError> {
        Ok(self.files.keys().cloned().collect())
    }
    fn read(&self, path: &str) -> Result<Option<Vec<u8>>, LocalError> {
        Ok(self.files.get(path).cloned())
    }
    fn write_batch(&mut self, files: &[(String, Vec<u8>)]) -> Result<(), LocalError> {
        self.batches += 1;
        for (p, b) in files {
            self.files.insert(p.clone(), b.clone());
        }
        Ok(())
    }
    fn delete(&mut self, paths: &[String]) -> Result<(), LocalError> {
        for p in paths {
            self.files.remove(p);
            self.exec.remove(p);
        }
        Ok(())
    }
    fn is_exec(&self, path: &str) -> Result<bool, LocalError> {
        Ok(self.exec.contains(path))
    }
    fn set_exec(&mut self, paths: &[String]) -> Result<(), LocalError> {
        self.exec.extend(paths.iter().cloned());
        Ok(())
    }
}

/// 引擎看到的是远端逻辑路径（设置带组前缀、主题带类别前缀）；本适配器在读写本机时映射回本机路径。
pub struct NsLocal<'a> {
    pub inner: &'a mut dyn LocalStore,
    pub ns: Namespace,
}

fn unmapped(p: &str) -> LocalError {
    LocalError(format!("路径不属于本机命名空间: {p}"))
}

impl LocalStore for NsLocal<'_> {
    fn list(&self) -> Result<Vec<String>, LocalError> {
        Ok(self
            .inner
            .list()?
            .iter()
            .map(|p| to_remote(p, &self.ns))
            .collect())
    }
    fn read(&self, path: &str) -> Result<Option<Vec<u8>>, LocalError> {
        match to_local(path, &self.ns) {
            Some(l) => self.inner.read(&l),
            None => Ok(None),
        }
    }
    fn write_batch(&mut self, files: &[(String, Vec<u8>)]) -> Result<(), LocalError> {
        let mapped = files
            .iter()
            .map(|(p, b)| Ok((to_local(p, &self.ns).ok_or_else(|| unmapped(p))?, b.clone())))
            .collect::<Result<Vec<_>, LocalError>>()?;
        self.inner.write_batch(&mapped)
    }
    fn delete(&mut self, paths: &[String]) -> Result<(), LocalError> {
        let mapped = paths
            .iter()
            .map(|p| to_local(p, &self.ns).ok_or_else(|| unmapped(p)))
            .collect::<Result<Vec<_>, _>>()?;
        self.inner.delete(&mapped)
    }
    fn is_exec(&self, path: &str) -> Result<bool, LocalError> {
        match to_local(path, &self.ns) {
            Some(l) => self.inner.is_exec(&l),
            None => Ok(false),
        }
    }
    fn set_exec(&mut self, paths: &[String]) -> Result<(), LocalError> {
        let mapped = paths
            .iter()
            .map(|p| to_local(p, &self.ns).ok_or_else(|| unmapped(p)))
            .collect::<Result<Vec<_>, _>>()?;
        self.inner.set_exec(&mapped)
    }
}
