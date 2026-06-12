// Command dicttool 提供词库格式转换工具。
//
// 目前支持把 rime `.dict.yaml` 拆分为原生 split 格式 `.dict.toml` + `.dict.tsv`：
// 在 header 结束标记 `...` 处切开，头部转 TOML、数据体逐字节写入 TSV（无损）。
//
// 用法：
//
//	dicttool split <a.dict.yaml> [b.dict.yaml ...]   # 输出到各源文件同目录
//	dicttool split -o <outDir> <a.dict.yaml> ...      # 输出到指定目录
package main

import (
	"flag"
	"fmt"
	"os"

	"github.com/huanfeng/wind_input/internal/dict/dictcache"
)

func main() {
	if len(os.Args) < 2 {
		usage()
		os.Exit(2)
	}

	switch os.Args[1] {
	case "split":
		runSplit(os.Args[2:])
	default:
		usage()
		os.Exit(2)
	}
}

func runSplit(args []string) {
	fs := flag.NewFlagSet("split", flag.ExitOnError)
	outDir := fs.String("o", "", "输出目录（默认与源文件同目录）")
	_ = fs.Parse(args)

	paths := fs.Args()
	if len(paths) == 0 {
		usage()
		os.Exit(2)
	}

	var failed int
	for _, p := range paths {
		tomlPath, tsvPath, err := dictcache.ConvertRimeYAMLToSplit(p, *outDir)
		if err != nil {
			fmt.Fprintf(os.Stderr, "转换失败 %s: %v\n", p, err)
			failed++
			continue
		}
		fmt.Printf("%s\n  → %s\n  → %s\n", p, tomlPath, tsvPath)
	}
	if failed > 0 {
		os.Exit(1)
	}
}

func usage() {
	fmt.Fprint(os.Stderr, `dicttool — 词库格式转换工具

用法:
  dicttool split [-o outDir] <a.dict.yaml> [b.dict.yaml ...]
      把 rime .dict.yaml 拆为 split 格式 .dict.toml + .dict.tsv（无损切分）
`)
}
