<!-- Parent: ../../AGENTS.md -->
<!-- Updated: 2026-08-14 -->

# wind-webdata

## Purpose
设置页数据 RPC（schema/dict/temp/freq/shadow/phrase/stats/theme 命名空间，方法名与前端 contract.ts 1:1）。由 wind-coordinator 的 webdata 模块独立而来：RPC 本体是 `WebDataRpc: WebDataHost` trait 的默认方法，只能经 `wind_coordinator::web_host::WebDataHost` 窄面（16 方法）触宿主——默认方法看不见 Coordinator 字段，窄面约束由编译器守门。

## 关键约束
- **依赖方向：本 crate → wind-coordinator**，绝不能反向。coordinator 不依赖本 crate 的 wind-transfer/fontdb，移动端闭包（wind-mobile 不依赖本 crate）因此无任何 C 依赖，`cargo check-android` 本机免 NDK 可跑。
- 新增 RPC 需要新宿主能力时**加在 WebDataHost 上**（coordinator/src/web_host.rs），勿在默认方法里绕道。
- 调用方须 `use wind_webdata::WebDataRpc`（service 的 RpcCore、coordinator 的集成测试经 dev-dependencies 环）。
- `theme.renderPreview(s)`（主题预览图 + 磁盘缓存）在 `src/theme_render.rs`：出图经窄面 `request_candidate_preview` 投给 UI 线程，本 crate 只管参数、主题来源、缓存键与清理（协议与阈值见 `docs/design/wind-scene-extraction.md` §7）。测试缓存一律传临时目录，别走 `Config::cache_dir()`。
- ⛔ 不用 blanket impl（`impl<T: WebDataHost> WebDataRpc for T`）：`Arc<Coordinator>` 的方法解析会先命中 T=Arc 的 blanket 候选、bound 不满足即报错而不再 deref——具体 impl 才让 Arc 自动 deref。

## Testing
- 本 crate 内嵌契约测试（真实 Coordinator + 临时 redb store）：白盒点走 WebDataHost 窄面与 coordinator 的 `debug_*` 支撑（debug_support.rs）。
- 零 RPC 的行为测试（词频路由/造词/加词）**不属于这里**，在 coordinator 的 `freq_learn_tests.rs`——分拣判据是「是否调 web_data_rpc」。
