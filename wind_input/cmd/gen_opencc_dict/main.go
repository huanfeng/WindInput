// gen_opencc_dict 把 OpenCC 文本词典编译为 .octrie 二进制格式。
//
// 用法:
//
//	gen_opencc_dict -src <OpenCC dictionaries dir> -out <output dir>
//
// 输入：OpenCC 上游 data/dictionary/*.txt（如 STCharacters.txt、STPhrases.txt、
// TWVariants.txt、TWPhrases.txt、HKVariants.txt）。
//
// 输出：每个 .txt 对应一个 <Name>.octrie，写入 -out 目录。
//
// 文本格式（OpenCC 通用）：
//
//	简体    繁体1 繁体2 ...
//
// 列之间以 \t 分隔，多个 value 用空格分隔。本工具只取第一个 value。
// 空行、以 # 开头的注释行、以及 key 为空的行被忽略。
package main

import (
	"bufio"
	"encoding/binary"
	"flag"
	"fmt"
	"os"
	"path/filepath"
	"sort"
	"strings"

	"github.com/huanfeng/wind_input/internal/transform/s2t"
)

// 编译时处理的 OpenCC 文件清单。涵盖 s2t / s2tw / s2twp / s2hk 链路所需。
// 注：OpenCC 上游已将原 TWPhrasesIT/Name/Other 合并为单一 TWPhrases.txt。
var dictFiles = []string{
	"STCharacters.txt",
	"STPhrases.txt",
	"TWVariants.txt",
	"TWPhrases.txt",
	"HKVariants.txt",
}

type kv struct {
	key string
	val string
}

func main() {
	srcDir := flag.String("src", "", "OpenCC 词典源目录（包含 *.txt 文件）")
	outDir := flag.String("out", "", "输出目录（写入 *.octrie）")
	flag.Parse()

	if *srcDir == "" || *outDir == "" {
		flag.Usage()
		os.Exit(1)
	}

	if err := os.MkdirAll(*outDir, 0o755); err != nil {
		fatalf("mkdir %s: %v", *outDir, err)
	}

	for _, f := range dictFiles {
		srcPath := filepath.Join(*srcDir, f)
		if _, err := os.Stat(srcPath); err != nil {
			fmt.Printf("[skip] %s: %v\n", f, err)
			continue
		}
		name := strings.TrimSuffix(f, ".txt")
		outPath := filepath.Join(*outDir, name+".octrie")

		entries, err := loadOpenCC(srcPath)
		if err != nil {
			fatalf("load %s: %v", srcPath, err)
		}
		if err := writeDict(outPath, entries); err != nil {
			fatalf("write %s: %v", outPath, err)
		}
		fmt.Printf("[ok] %-20s -> %s (%d entries)\n", f, filepath.Base(outPath), len(entries))
	}
}

func loadOpenCC(path string) ([]kv, error) {
	file, err := os.Open(path)
	if err != nil {
		return nil, err
	}
	defer file.Close()

	scanner := bufio.NewScanner(file)
	scanner.Buffer(make([]byte, 0, 1024*1024), 4*1024*1024)

	seen := make(map[string]struct{}, 4096)
	out := make([]kv, 0, 4096)

	for scanner.Scan() {
		line := scanner.Text()
		if line == "" || line[0] == '#' {
			continue
		}
		tab := strings.IndexByte(line, '\t')
		if tab <= 0 {
			continue
		}
		key := strings.TrimSpace(line[:tab])
		rest := strings.TrimSpace(line[tab+1:])
		if key == "" || rest == "" {
			continue
		}
		// 第一个 value（空格切分）
		val := rest
		if sp := strings.IndexByte(rest, ' '); sp > 0 {
			val = rest[:sp]
		}
		if val == "" || val == key {
			// key == val 没有转换意义，丢弃
			continue
		}
		if _, dup := seen[key]; dup {
			continue
		}
		seen[key] = struct{}{}
		out = append(out, kv{key: key, val: val})
	}
	if err := scanner.Err(); err != nil {
		return nil, err
	}

	// 按 key 字节序升序，加载侧二分依赖此次序
	sort.Slice(out, func(i, j int) bool { return out[i].key < out[j].key })
	return out, nil
}

func writeDict(path string, entries []kv) error {
	// 构建 string table（共享池），并记录每条 key/val 的偏移与长度
	type meta struct {
		keyOff, valOff uint32
		keyLen, valLen uint16
	}
	metas := make([]meta, len(entries))
	st := make([]byte, 0, 16*1024)

	maxKey := 0
	for i, e := range entries {
		kb := []byte(e.key)
		vb := []byte(e.val)
		if len(kb) > 0xFFFF || len(vb) > 0xFFFF {
			return fmt.Errorf("entry too long: %q -> %q", e.key, e.val)
		}
		if len(kb) > maxKey {
			maxKey = len(kb)
		}
		metas[i].keyOff = uint32(len(st))
		metas[i].keyLen = uint16(len(kb))
		st = append(st, kb...)
		metas[i].valOff = uint32(len(st))
		metas[i].valLen = uint16(len(vb))
		st = append(st, vb...)
	}
	if maxKey > 0xFFFF {
		return fmt.Errorf("max key length exceeds uint16: %d", maxKey)
	}

	f, err := os.Create(path)
	if err != nil {
		return err
	}
	defer f.Close()
	w := bufio.NewWriter(f)

	// Header
	if _, err := w.Write([]byte(s2t.FormatMagic)); err != nil {
		return err
	}
	if err := binary.Write(w, binary.LittleEndian, s2t.FormatVersion); err != nil {
		return err
	}
	if err := binary.Write(w, binary.LittleEndian, uint32(len(entries))); err != nil {
		return err
	}
	if err := binary.Write(w, binary.LittleEndian, uint16(maxKey)); err != nil {
		return err
	}
	if err := binary.Write(w, binary.LittleEndian, uint16(0)); err != nil {
		return err
	}
	// Entry table
	for _, m := range metas {
		if err := binary.Write(w, binary.LittleEndian, m.keyOff); err != nil {
			return err
		}
		if err := binary.Write(w, binary.LittleEndian, m.keyLen); err != nil {
			return err
		}
		if err := binary.Write(w, binary.LittleEndian, m.valOff); err != nil {
			return err
		}
		if err := binary.Write(w, binary.LittleEndian, m.valLen); err != nil {
			return err
		}
	}
	// String table
	if _, err := w.Write(st); err != nil {
		return err
	}
	return w.Flush()
}

func fatalf(format string, args ...interface{}) {
	fmt.Fprintf(os.Stderr, format+"\n", args...)
	os.Exit(1)
}
