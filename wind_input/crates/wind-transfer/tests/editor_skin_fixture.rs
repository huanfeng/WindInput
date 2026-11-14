//! 跨仓夹具：主题编辑器导出的带皮肤主题包（见 `testdata/README.md`）能被导入读回。
//! 编辑器那侧的 `validateThemePackage` 按本 crate 的口径自检；这里钉住另一半——编辑器真打出来的包
//! 引擎收得下，且资源原样落盘。

use std::path::Path;

use wind_transfer::theme::{import_package, preview_package};

const FIXTURE: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/testdata/editor-skin.wtheme");

#[test]
fn preview_reads_editor_skin_package() {
    let p = preview_package(Path::new(FIXTURE)).expect("编辑器导出的包应能预览");
    assert_eq!(p.display_name, "编辑器皮肤夹具");
    assert_eq!(p.author, "WindInputThemeEditor");
    assert_eq!(p.version, "1.0");
    assert_eq!(p.asset_count, 3);
    assert!(!p.has_preview);
}

#[test]
fn import_places_skin_assets_verbatim() {
    let tmp = tempfile::tempdir().unwrap();
    let dst = tmp.path().join("editor-skin");
    let out = import_package(Path::new(FIXTURE), &dst, |dir| {
        assert!(dir.join("theme.toml").exists());
        Ok(())
    })
    .expect("编辑器导出的包应能导入");
    let mut files = out.files.clone();
    files.sort();
    assert_eq!(
        files,
        [
            "assets/dot.png",
            "assets/frame.png",
            "assets/frame_v.png",
            "theme.toml"
        ]
    );
    // 包里的字节原样落盘（存储模式条目不经再压缩）。
    let file = std::fs::File::open(FIXTURE).unwrap();
    let mut zip = zip::ZipArchive::new(file).unwrap();
    for name in ["assets/dot.png", "assets/frame.png", "assets/frame_v.png"] {
        let mut entry = zip.by_name(name).unwrap();
        assert_eq!(
            entry.compression(),
            zip::CompressionMethod::Stored,
            "{name}"
        );
        let mut want = Vec::new();
        std::io::Read::read_to_end(&mut entry, &mut want).unwrap();
        assert_eq!(std::fs::read(dst.join(name)).unwrap(), want, "{name}");
    }
    let text = std::fs::read_to_string(dst.join("theme.toml")).unwrap();
    assert!(text.contains("assets/frame.png") && !text.contains("data:"));
}
