// perf_recall 用于评估首键候选裁剪带来的召回损失与耗时。
//
// 对比同一个 layer.LookupPrefix 在两种 limit 下的 top-N 结果：
//   - "limited"  ：模拟生产路径（CompositeDict.prefixSafeLimit），limit 默认 200
//   - "ground"   ：limit=0，扫描全部候选并完整排序
//
// 输出每个首字母的 top-N 重合度（Jaccard 计算）以及多次查询的中位耗时。
//
// 用法：
//
//	go run ./tools/perf_recall \
//	    --wdb  "%LOCALAPPDATA%/WindInput/cache/wubi86.wdb" \
//	    --wdat "%LOCALAPPDATA%/WindInput/cache/pinyin.wdat" \
//	    --top 50 --limited 200 --repeat 5
package main

import (
	"flag"
	"fmt"
	"os"
	"sort"
	"time"

	"github.com/huanfeng/wind_input/internal/candidate"
	"github.com/huanfeng/wind_input/internal/dict/binformat"
	"github.com/huanfeng/wind_input/internal/dict/datformat"
)

// prefixQuerier 抽象 binformat.DictReader / datformat.WdatReader 的共同前缀接口
type prefixQuerier interface {
	LookupPrefix(prefix string, limit int) []candidate.Candidate
}

func main() {
	wdbPath := flag.String("wdb", "", "wubi86.wdb / flypy.wdb 路径")
	wdatPath := flag.String("wdat", "", "pinyin.wdat 路径")
	top := flag.Int("top", 50, "对比的 top-N")
	limited := flag.Int("limited", 200, "模拟生产路径的 layer 裁剪上限")
	letters := flag.String("letters", "abcdefghijklmnopqrstuvwxyz", "要测试的首键集合")
	repeat := flag.Int("repeat", 5, "每个首字母测多少次取耗时中位数")
	flag.Parse()

	if *wdbPath == "" && *wdatPath == "" {
		fmt.Fprintln(os.Stderr, "至少指定 --wdb 或 --wdat")
		os.Exit(2)
	}

	if *wdbPath != "" {
		r, err := binformat.OpenDict(*wdbPath)
		if err != nil {
			fmt.Fprintf(os.Stderr, "OpenDict: %v\n", err)
			os.Exit(1)
		}
		fmt.Printf("\n=== %s (binformat.DictReader) ===\n", *wdbPath)
		eval(r, *letters, *limited, *top, *repeat)
		r.Close()
	}
	if *wdatPath != "" {
		r, err := datformat.OpenWdat(*wdatPath)
		if err != nil {
			fmt.Fprintf(os.Stderr, "OpenWdat: %v\n", err)
			os.Exit(1)
		}
		fmt.Printf("\n=== %s (datformat.WdatReader) ===\n", *wdatPath)
		eval(r, *letters, *limited, *top, *repeat)
		r.Close()

		// 验证 hotcache 进程级共享：再开一次同一文件的新 reader，
		// 首次单字母查询应直接命中缓存（无重建成本）。
		fmt.Printf("\n=== hotcache 共享验证：重新打开 %s 后第一次单字母查询耗时 ===\n", *wdatPath)
		r2, err := datformat.OpenWdat(*wdatPath)
		if err == nil {
			for _, ch := range []rune("syz") {
				t := time.Now()
				cands := r2.LookupPrefix(string(ch), *limited)
				ms := time.Since(t).Seconds() * 1000
				fmt.Printf("  '%s' first call: %.3fms, %d candidates\n", string(ch), ms, len(cands))
			}
			r2.Close()
		}
	}
}

func eval(q prefixQuerier, letters string, limited, top, repeat int) {
	printRow([]string{"letter", "ground_n", "limited_n", "top_overlap", "limited_first_ms", "limited_p50_ms", "ground_p50_ms"})
	totalOverlap := 0.0
	totalGround := 0.0
	totalLimited := 0.0
	totalLimitedFirst := 0.0
	count := 0
	for _, ch := range letters {
		letter := string(ch)
		gMsList := make([]float64, 0, repeat)
		lMsList := make([]float64, 0, repeat)
		var ground, limitedRes []candidate.Candidate

		// 先单独测一次 limited 的首次调用：第一次访问该字母时会触发 hot index 构建（冷启动成本）
		// 所以放在 ground 之前，才能看到真正的"cold"耗时
		tFirst := time.Now()
		limitedRes = q.LookupPrefix(letter, limited)
		firstMs := time.Since(tFirst).Seconds() * 1000

		for i := 0; i < repeat; i++ {
			t1 := time.Now()
			ground = q.LookupPrefix(letter, 0)
			gMsList = append(gMsList, time.Since(t1).Seconds()*1000)

			t2 := time.Now()
			limitedRes = q.LookupPrefix(letter, limited)
			lMsList = append(lMsList, time.Since(t2).Seconds()*1000)
		}
		gMs := median(gMsList)
		lMs := median(lMsList)
		overlap := topNOverlap(ground, limitedRes, top)
		printRow([]string{
			letter,
			fmt.Sprintf("%d", len(ground)),
			fmt.Sprintf("%d", len(limitedRes)),
			fmt.Sprintf("%.3f", overlap),
			fmt.Sprintf("%.2f", firstMs),
			fmt.Sprintf("%.2f", lMs),
			fmt.Sprintf("%.2f", gMs),
		})
		totalOverlap += overlap
		totalGround += gMs
		totalLimited += lMs
		totalLimitedFirst += firstMs
		count++
	}
	if count > 0 {
		fmt.Printf("avg overlap: %.3f, limited first=%.2fms, limited p50=%.2fms, ground p50=%.2fms over %d letters\n",
			totalOverlap/float64(count), totalLimitedFirst/float64(count),
			totalLimited/float64(count), totalGround/float64(count), count)
	}
}

func median(v []float64) float64 {
	c := make([]float64, len(v))
	copy(c, v)
	sort.Float64s(c)
	n := len(c)
	if n == 0 {
		return 0
	}
	if n%2 == 1 {
		return c[n/2]
	}
	return (c[n/2-1] + c[n/2]) / 2
}

// topNOverlap 计算两个候选列表的 top-N 集合相似度。
// ground 完整全量按 candidate.Better 排序后取前 N；got 直接取前 N。
// 用 (Text, Code) 二元组作为键。
func topNOverlap(ground, got []candidate.Candidate, n int) float64 {
	if n <= 0 {
		return 1
	}
	gSorted := make([]candidate.Candidate, len(ground))
	copy(gSorted, ground)
	sort.SliceStable(gSorted, func(i, j int) bool { return candidate.Better(gSorted[i], gSorted[j]) })
	gTop := gSorted
	if len(gTop) > n {
		gTop = gTop[:n]
	}
	xTop := got
	if len(xTop) > n {
		xTop = xTop[:n]
	}
	if len(gTop) == 0 && len(xTop) == 0 {
		return 1
	}
	type k struct{ t, c string }
	set := make(map[k]struct{}, len(gTop))
	for _, c := range gTop {
		set[k{c.Text, c.Code}] = struct{}{}
	}
	hit := 0
	for _, c := range xTop {
		if _, ok := set[k{c.Text, c.Code}]; ok {
			hit++
		}
	}
	denom := len(gTop)
	if len(xTop) > denom {
		denom = len(xTop)
	}
	return float64(hit) / float64(denom)
}

func printRow(cols []string) {
	for i, s := range cols {
		if i > 0 {
			fmt.Print("  ")
		}
		fmt.Printf("%-14s", s)
	}
	fmt.Println()
}
