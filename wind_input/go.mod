module github.com/huanfeng/wind_input

go 1.25.0

toolchain go1.25.6

require (
	github.com/Microsoft/go-winio v0.6.2
	github.com/gogpu/gg v0.44.1
	github.com/google/uuid v1.6.0
	go.etcd.io/bbolt v1.4.3
	golang.org/x/sys v0.43.0
	gopkg.in/yaml.v3 v3.0.1
)

require (
	github.com/go-text/typesetting v0.3.4 // indirect
	github.com/gogpu/gpucontext v0.18.0 // indirect
	github.com/gogpu/gputypes v0.5.0 // indirect
	golang.org/x/image v0.40.0 // indirect
	golang.org/x/text v0.37.0 // indirect
)

// 临时指向 fork (feat/external-buffer-and-image-view), 增加 NewPixmapFromBuffer
// 与 (*Pixmap).ImageView 用于零拷贝缓冲复用。待 upstream gogpu/gg PR 合并后撤销。
replace github.com/gogpu/gg => github.com/huanfeng/gg v0.47.3-0.20260521041445-29e9f420335f
