# wind-transfer 测试夹具

| 文件 | 来源 | 用途 |
|---|---|---|
| `editor-skin.wtheme` | 主题编辑器仓 `WindInputThemeEditor` 的 `src/lib/theme3/testdata/editor-skin.wtheme`（`pnpm gen:skin-fixture` 生成，逻辑在 `src/lib/theme3/skinFixture.ts`） | `tests/editor_skin_fixture.rs`：编辑器导出的带皮肤主题包（PNG 存储模式、`[skin]` 直接引用与经 `[resources]` 键引用）能被 `preview_package` / `import_package` 读回 |

编辑器那边重新生成后，把新文件复制过来覆盖同名文件。图片全部程序生成，不含第三方素材。
