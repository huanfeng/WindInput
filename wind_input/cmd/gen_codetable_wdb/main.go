package main

import (
	"flag"
	"log"
	"log/slog"
	"path/filepath"

	"github.com/huanfeng/wind_input/internal/dict/dictcache"
)

func main() {
	srcPath := flag.String("src", "dict/wubi86/wubi86.txt", "五笔码表源文件路径")
	outDir := flag.String("out", "dict/wubi86", "输出目录")
	flag.Parse()

	wdbPath := filepath.Join(*outDir, "codetable.wdb")

	if err := dictcache.ConvertCodeTableToWdb(*srcPath, wdbPath, slog.Default()); err != nil {
		log.Fatalf("生成 codetable.wdb 失败: %v", err)
	}

	log.Printf("codetable.wdb 生成完毕 → %s", wdbPath)
}
