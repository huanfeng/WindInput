package theme

// svg.go — 矢量图（SVG）按目标设备像素现场栅格化。
// 纯 Go（oksvg + rasterx，无 cgo）。矢量随尺寸栅格化 → 任意 DPI 清晰；
// 调用方（internal/ui imageResolver）按设备像素尺寸缓存结果，避免每帧重栅格化。

import (
	"bytes"
	"encoding/base64"
	"fmt"
	"image"
	"net/url"
	"os"
	"strings"

	"github.com/srwiley/oksvg"
	"github.com/srwiley/rasterx"
)

// IsSVGRef 判断 ref 是否 SVG：`.svg` 文件路径或 `data:image/svg+xml` data URI。
func IsSVGRef(ref string) bool {
	s := strings.ToLower(strings.TrimSpace(ref))
	return strings.HasSuffix(s, ".svg") || strings.HasPrefix(s, "data:image/svg+xml")
}

// loadSVGBytes 从文件路径或 data: URI 取 SVG 原始字节（支持 base64 / URL 编码 / 原文）。
func loadSVGBytes(pathOrDataURI string) ([]byte, error) {
	if pathOrDataURI == "" {
		return nil, fmt.Errorf("empty svg ref")
	}
	if strings.HasPrefix(pathOrDataURI, "data:") {
		comma := strings.IndexByte(pathOrDataURI, ',')
		if comma < 0 {
			return nil, fmt.Errorf("invalid svg data URI: missing comma")
		}
		header, body := pathOrDataURI[:comma], pathOrDataURI[comma+1:]
		if strings.Contains(header, ";base64") {
			return base64.StdEncoding.DecodeString(body)
		}
		if dec, err := url.PathUnescape(body); err == nil {
			return []byte(dec), nil
		}
		return []byte(body), nil
	}
	return os.ReadFile(pathOrDataURI)
}

// RasterizeSVG 把 SVG（文件路径或 data URI）按目标设备像素 w×h 栅格化为预乘 alpha 的 *image.RGBA。
func RasterizeSVG(pathOrDataURI string, w, h int) (*image.RGBA, error) {
	if w <= 0 || h <= 0 {
		return nil, fmt.Errorf("RasterizeSVG: size must be >0, got %dx%d", w, h)
	}
	data, err := loadSVGBytes(pathOrDataURI)
	if err != nil {
		return nil, fmt.Errorf("load svg: %w", err)
	}
	icon, err := oksvg.ReadIconStream(bytes.NewReader(data))
	if err != nil {
		return nil, fmt.Errorf("parse svg: %w", err)
	}
	icon.SetTarget(0, 0, float64(w), float64(h))
	rgba := image.NewRGBA(image.Rect(0, 0, w, h))
	scanner := rasterx.NewScannerGV(w, h, rgba, rgba.Bounds())
	raster := rasterx.NewDasher(w, h, scanner)
	icon.Draw(raster, 1.0)
	return rgba, nil
}
