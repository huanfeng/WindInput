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
    /// 一轮的写入与删除合成一次调用（真实实现据此一轮只暂停/恢复扩展一次）；
    /// `exec` 是写入中要带可执行位的路径，真实实现在落位前设好。
    fn apply(
        &mut self,
        writes: &[(String, Vec<u8>)],
        exec: &[String],
        deletes: &[String],
    ) -> Result<(), LocalError> {
        apply_in_steps(self, writes, exec, deletes)
    }
    /// 文件字节数（None = 不存在或不知道）：引擎据此跳过超大文件而不读入。
    fn size(&self, _path: &str) -> Result<Option<u64>, LocalError> {
        Ok(None)
    }
    /// 文件是否带可执行位（Windows 恒为 false）。
    fn is_exec(&self, _path: &str) -> Result<bool, LocalError> {
        Ok(false)
    }
    /// 给这些文件加上可执行位（保留其余权限位；Windows 忽略）。
    fn set_exec(&mut self, _paths: &[String]) -> Result<(), LocalError> {
        Ok(())
    }
}

/// `apply` 的默认拆法：写入、补可执行位、删除依次各调一次。
fn apply_in_steps<S: LocalStore + ?Sized>(
    s: &mut S,
    writes: &[(String, Vec<u8>)],
    exec: &[String],
    deletes: &[String],
) -> Result<(), LocalError> {
    if !writes.is_empty() {
        s.write_batch(writes)?;
    }
    if !exec.is_empty() {
        s.set_exec(exec)?;
    }
    if !deletes.is_empty() {
        s.delete(deletes)?;
    }
    Ok(())
}

/// 内存实现（测试与 S4 之前的联调）。
#[derive(Debug, Clone, Default)]
pub struct MemLocal {
    pub files: BTreeMap<String, Vec<u8>>,
    /// 累计 write_batch 调用次数（断言「一批写入合并成一次」）。
    pub batches: usize,
    /// 累计 apply 调用次数（断言「一轮只一次」）。
    pub applies: usize,
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
    fn apply(
        &mut self,
        writes: &[(String, Vec<u8>)],
        exec: &[String],
        deletes: &[String],
    ) -> Result<(), LocalError> {
        self.applies += 1;
        apply_in_steps(self, writes, exec, deletes)
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

impl NsLocal<'_> {
    fn map(&self, p: &str) -> Result<String, LocalError> {
        to_local(p, &self.ns).ok_or_else(|| unmapped(p))
    }
    fn map_all(&self, paths: &[String]) -> Result<Vec<String>, LocalError> {
        paths.iter().map(|p| self.map(p)).collect()
    }
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
    fn apply(
        &mut self,
        writes: &[(String, Vec<u8>)],
        exec: &[String],
        deletes: &[String],
    ) -> Result<(), LocalError> {
        let writes = writes
            .iter()
            .map(|(p, b)| Ok((self.map(p)?, b.clone())))
            .collect::<Result<Vec<_>, LocalError>>()?;
        let exec = self.map_all(exec)?;
        let deletes = self.map_all(deletes)?;
        self.inner.apply(&writes, &exec, &deletes)
    }
    fn size(&self, path: &str) -> Result<Option<u64>, LocalError> {
        match to_local(path, &self.ns) {
            Some(l) => self.inner.size(&l),
            None => Ok(None),
        }
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
