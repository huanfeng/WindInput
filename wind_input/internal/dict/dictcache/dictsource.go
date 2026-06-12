package dictcache

// dictsource.go —— 词库源格式抽象层。
//
// 词库支持两种磁盘格式：
//   - rime `.dict.yaml`：YAML 头 + TSV 数据体，单文件（rime 生态交换格式）。
//   - 原生 split：`.dict.toml`（纯头）+ 同 stem `.dict.tsv`（制表符分隔数据体）。
//
// 中间层 = 类型化 DictHeader：两种格式各自原生解码进同一结构（yaml.v3 / go-toml/v2，
// 双 tag），互不经过对方。OpenDictSource 把"头来源"与"体来源"解耦为统一表示，
// 上层体解析逻辑（loadRimeCodetableFile / loadRimeFile）消费同一个 io.Reader，
// 两种格式零行为差异。

import (
	"bufio"
	"io"
	"os"
	"path/filepath"
	"strings"

	toml "github.com/pelletier/go-toml/v2"
	"gopkg.in/yaml.v3"
)

// 词库文件后缀。
const (
	dictSuffixYAML = ".dict.yaml" // rime 单文件格式（头+体）
	dictSuffixTOML = ".dict.toml" // split 头文件
	dictSuffixTSV  = ".dict.tsv"  // split 数据体文件（制表符分隔）
)

// DictHeader 词库头（rime YAML header 与 split TOML 头的统一表示）。
// columns 缺省时按 rime 标准 text/code/weight 顺序解析；split 与 yaml 共用此结构。
type DictHeader struct {
	Name         string   `yaml:"name" toml:"name,omitempty"`
	Version      string   `yaml:"version" toml:"version,omitempty"`
	Sort         string   `yaml:"sort" toml:"sort,omitempty"`
	Columns      []string `yaml:"columns" toml:"columns,omitempty"`
	ImportTables []string `yaml:"import_tables" toml:"import_tables,omitempty"`
}

// isSplitDictPath 判断词库主路径是否为 split 格式（.dict.toml）。
func isSplitDictPath(path string) bool {
	return strings.HasSuffix(path, dictSuffixTOML)
}

// dictStem 去掉词库后缀（.dict.toml/.dict.yaml/.dict.tsv）返回 stem（含目录）。
// 无已知后缀时原样返回。
func dictStem(path string) string {
	for _, suf := range []string{dictSuffixTOML, dictSuffixYAML, dictSuffixTSV} {
		if s, ok := strings.CutSuffix(path, suf); ok {
			return s
		}
	}
	return path
}

// dictSuffixOf 返回与主词库同格式的词库后缀（split→.dict.toml，否则→.dict.yaml）。
// 用于把 import_tables 名解析为兄弟词库路径，扩展名跟随主词库格式。
func dictSuffixOf(mainDictPath string) string {
	if isSplitDictPath(mainDictPath) {
		return dictSuffixTOML
	}
	return dictSuffixYAML
}

// dictFilesFor 返回构成一个词库的磁盘文件列表，用于缓存失效检测：
// rime 为单文件 [.dict.yaml]；split 为 [.dict.toml, .dict.tsv]（tsv 存在才纳入，
// 保证 meta.Sources 列表稳定）。任一文件变更都应触发 wdb/wdat 重建。
func dictFilesFor(dictPath string) []string {
	if !isSplitDictPath(dictPath) {
		return []string{dictPath}
	}
	out := []string{dictPath}
	tsv := dictStem(dictPath) + dictSuffixTSV
	if _, err := os.Stat(tsv); err == nil {
		out = append(out, tsv)
	}
	return out
}

// bodyReadCloser 组合数据体 Reader 与底层文件 Closer。
// yaml 路径：Reader = 定位到 `...` 之后的 bufio.Reader，closer = 底层文件；
// split 路径：Reader 与 closer 同为 .dict.tsv 文件。
type bodyReadCloser struct {
	io.Reader
	closer io.Closer
}

func (b bodyReadCloser) Close() error {
	if b.closer != nil {
		return b.closer.Close()
	}
	return nil
}

// scanRimeYAMLHeader 从 br 读取 rime YAML header（读到 `...` 结束标记为止；
// 起始 `---` 可选），解析为 DictHeader。读取完成后 br 恰好定位到数据体首行。
// header 解析错误被容忍（降级为已部分填充/空的 DictHeader），与旧版逐行解析的
// 宽容语义一致——外部 rime 词库头偶有非常规内容不应中断转换。
func scanRimeYAMLHeader(br *bufio.Reader) DictHeader {
	var headerBuf strings.Builder
	for {
		line, err := br.ReadString('\n')
		if len(line) > 0 {
			trimmed := strings.TrimSpace(line)
			if trimmed == "..." {
				break
			}
			// `---` 是 YAML 文档起始标记，不计入 header 内容（避免空文档干扰）。
			if trimmed != "---" {
				headerBuf.WriteString(line)
			}
		}
		if err != nil {
			break // EOF 或读取错误：无 `...` 分隔符，整段视为 header、数据体为空
		}
	}
	var hdr DictHeader
	if headerBuf.Len() > 0 {
		_ = yaml.Unmarshal([]byte(headerBuf.String()), &hdr)
	}
	return hdr
}

// OpenDictSource 按格式打开词库源，返回解析好的头与定位到数据体首行的流。
// 调用方负责 Close 返回的 body。
//
//	.dict.yaml：读到 `...` 为止解析头；body = 同一文件 `...` 之后的续流（单次 open、保持流式）。
//	.dict.toml：整文件解析头；body = 同 stem 的 .dict.tsv 文件流（命名约定强制配对）。
func OpenDictSource(path string) (DictHeader, io.ReadCloser, error) {
	if isSplitDictPath(path) {
		return openSplitDictSource(path)
	}
	f, err := os.Open(path)
	if err != nil {
		return DictHeader{}, nil, err
	}
	br := bufio.NewReaderSize(f, 64*1024)
	hdr := scanRimeYAMLHeader(br)
	return hdr, bodyReadCloser{Reader: br, closer: f}, nil
}

// openSplitDictSource 打开 split 格式：头取自 .dict.toml，体取自同 stem 的 .dict.tsv。
func openSplitDictSource(tomlPath string) (DictHeader, io.ReadCloser, error) {
	data, err := os.ReadFile(tomlPath)
	if err != nil {
		return DictHeader{}, nil, err
	}
	var hdr DictHeader
	if err := toml.Unmarshal(data, &hdr); err != nil {
		return DictHeader{}, nil, err
	}
	tsvPath := dictStem(tomlPath) + dictSuffixTSV
	bf, err := os.Open(tsvPath)
	if err != nil {
		return DictHeader{}, nil, err
	}
	return hdr, bodyReadCloser{Reader: bf, closer: bf}, nil
}

// ReadDictHeader 仅解析词库头（不打开数据体），用于 import_tables / 源清单发现。
// 解析错误被容忍（返回空/部分头 + nil error），不中断发现流程。
func ReadDictHeader(path string) (DictHeader, error) {
	if isSplitDictPath(path) {
		data, err := os.ReadFile(path)
		if err != nil {
			return DictHeader{}, err
		}
		var hdr DictHeader
		_ = toml.Unmarshal(data, &hdr)
		return hdr, nil
	}
	f, err := os.Open(path)
	if err != nil {
		return DictHeader{}, err
	}
	defer f.Close()
	return scanRimeYAMLHeader(bufio.NewReaderSize(f, 64*1024)), nil
}

// ConvertRimeYAMLToSplit 把 rime .dict.yaml 拆成同 stem 的 split 格式
// .dict.toml（头）+ .dict.tsv（数据体）。在 header 结束标记 `...` 处切开：
// 头部解析为 DictHeader 写出 TOML；`...` 之后的数据体字节原样写入 .dict.tsv
// （无损，无需逐行重写/转义——tsv 与 rime body 逐字节同构）。
// outDir 为空时输出到源文件同目录。返回写出的两个文件路径。
func ConvertRimeYAMLToSplit(yamlPath, outDir string) (tomlPath, tsvPath string, err error) {
	f, err := os.Open(yamlPath)
	if err != nil {
		return "", "", err
	}
	defer f.Close()

	br := bufio.NewReaderSize(f, 64*1024)
	hdr := scanRimeYAMLHeader(br) // 消费 header，br 恰好定位到数据体首行

	stem := filepath.Base(dictStem(yamlPath))
	dir := outDir
	if dir == "" {
		dir = filepath.Dir(yamlPath)
	}
	tomlPath = filepath.Join(dir, stem+dictSuffixTOML)
	tsvPath = filepath.Join(dir, stem+dictSuffixTSV)

	hdrBytes, err := toml.Marshal(&hdr)
	if err != nil {
		return "", "", err
	}
	if err := os.WriteFile(tomlPath, hdrBytes, 0644); err != nil {
		return "", "", err
	}

	tsvFile, err := os.Create(tsvPath)
	if err != nil {
		return "", "", err
	}
	defer tsvFile.Close()
	if _, err := io.Copy(tsvFile, br); err != nil {
		return "", "", err
	}
	return tomlPath, tsvPath, nil
}
