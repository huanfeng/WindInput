<!-- Parent: ../AGENTS.md -->
<!-- Generated: 2026-04-08 | Updated: 2026-04-20 -->

# internal/dict/binformat

## Purpose
二进制词库文件格式（`.wdb`）的定义、读写和 mmap 支持。提供两种格式：

- **词库 wdb**（魔数 `WDIC`）：通用词库格式，包含主索引（code→entries）和简拼索引（abbrev→entries）。当前服务于码表（`wubi.wdb` 等，`internal/dict.CodeTable`）与英文词库（`english_dict.go`）。拼音词库已迁移到 DAT（`internal/dict/datformat`，`.wdat`），不再使用本格式
- **unigram.wdb**：Unigram 语言模型，魔数 `WUNI`，存储词语的对数概率

所有文件均为小端字节序，通过 mmap 映射到内存，实现近零堆内存占用。

## Key Files
| File | Description |
|------|-------------|
| `format.go` | 文件头、索引、条目的结构体定义和大小常量，以及 `Validate` 方法 |
| `reader.go` | `DictReader`：词库 wdb 的 mmap 读取器（`Lookup`、`LookupPrefix`、`LookupAbbrev`） |
| `writer.go` | `DictWriter`：词库 wdb 写入器（`AddCode`、`AddAbbrev`、`Write`、`SetLowMemory`） |
| `unigram_reader.go` | `UnigramReader`：unigram.wdb 的 mmap 读取器（`Lookup` 返回对数概率） |
| `unigram_writer.go` | `UnigramWriter`：unigram.wdb 写入器（`Add`、`Write`） |
| `shared.go` | 进程级共享 reader 池：`OpenDict`/`OpenUnigram` 按 FileKey（path+size+mtime）复用同一 mmap，引用计数管理生命周期 |
| `registry.go` | 强制关闭注册表：`CloseReadersForPath` 供 dictcache 在原子替换 wdb 前释放本进程 mmap 锁 |
| `mmap_windows.go` | Windows mmap 实现（`CreateFileMapping`/`MapViewOfFile`） |
| `binformat_test.go` | 读写往返测试 |
| `meta_test.go` | 元数据序列化测试 |

## For AI Agents

### Working In This Directory
- **不要修改文件格式常量**（`DictVersion`、`UnigramVersion`、结构体大小），否则需重新生成所有 `.wdb` 文件
- `DictFileHeaderSize=32`、`DictKeyIndexSize=12`、`DictEntryRecordSize=10`、`UnigramFileHeaderSize=24`、`UnigramKeyIndexSize=12` 均为固定大小
- AbbrevSection 在文件末尾，`AbbrevOff=0` 表示无简拼索引
- mmap 生命周期：`OpenDict`/`OpenUnigram` 是**进程级共享**的——同一文件（path+size+mtime 相同）多次打开返回同一 reader 实例并递增引用计数；`Close()` 必须**每持有者恰好调用一次**，仅最后一个持有者会真正解除映射。持有方若可能重复 Close，应在首次 Close 后将引用置 nil（参见 `CodeTable.Close`）。Reader 未全部关闭时不要删除文件
- 词库重建替换文件前调用 `CloseReadersForPath` 强制释放全进程同路径 mmap（绕过引用计数），被强关的 reader 查询安全返回空；文件替换后 mtime 变化生成新 FileKey，新旧 reader 自然隔离
- 写入器将字符串统一存入 StringPool，索引用偏移量引用，实现零拷贝读取
- `DictWriter.SetLowMemory(true)`：省内存写入路径，在 EntryRecords 构建完成后释放原始 `Entries` 切片并 GC（StringPool 已复制文本字节，释放安全）；仅降内存峰值，**不改变输出文件内容**。由 `dictcache` 在低内存机器上启用

### Testing Requirements
- `go test ./internal/dict/binformat/`
- 测试覆盖：写入后读取验证所有字段一致

### Common Patterns
- 生成工具：`cmd/gen_bindict`（unigram.wdb）、`cmd/gen_wubi_wdb`（wubi.wdb）；码表/英文 wdb 由运行时 `dictcache` 按需构建
- 运行时通过 `DictReader.Open(path)` 加载，返回后立即可查询

## Dependencies
### Internal
- 无（被 `internal/dict` 引用）

### External
- `golang.org/x/sys/windows` — mmap Windows API
- `encoding/binary` — 小端字节序读写

<!-- MANUAL: -->
