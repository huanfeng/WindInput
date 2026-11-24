use super::*;

/// 每一个 `STATUS_*` 的位值都必须与 C++ 侧 `BinaryProtocol.h` 逐位相同。
///
/// 两份常量各写各的，漂移时症状全是静默的（如「热键会话」被读成「软键盘开着」），
/// 所以直接解析头文件比对。新增 `STATUS_*` 时两侧都要加。
#[test]
fn status_bits_match_the_cpp_header() {
    let header = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../../wind_tsf/include/BinaryProtocol.h");
    let src = std::fs::read_to_string(&header)
        .unwrap_or_else(|e| panic!("读不到 {}: {e}", header.display()));

    // constexpr uint32_t STATUS_XXX = 0x0001; —— 只认这一种形态，多余空格随意。
    let mut cpp = std::collections::BTreeMap::new();
    for line in src.lines() {
        let line = line.trim();
        let Some(rest) = line.strip_prefix("constexpr uint32_t STATUS_") else {
            continue;
        };
        let Some((name, tail)) = rest.split_once('=') else {
            continue;
        };
        let Some(val) = tail.trim().trim_end_matches(';').split(';').next() else {
            continue;
        };
        let val = val.trim();
        let Some(hex) = val.strip_prefix("0x") else {
            continue;
        };
        let Ok(v) = u32::from_str_radix(hex, 16) else {
            continue;
        };
        cpp.insert(format!("STATUS_{}", name.trim()), v);
    }
    assert!(
        cpp.len() >= 9,
        "只从头文件解析出 {} 个 STATUS_* —— 多半是那边的写法变了，本测试已失效: {cpp:?}",
        cpp.len()
    );

    let rust: std::collections::BTreeMap<&str, u32> = [
        ("STATUS_CHINESE_MODE", STATUS_CHINESE_MODE),
        ("STATUS_FULL_WIDTH", STATUS_FULL_WIDTH),
        ("STATUS_CHINESE_PUNCT", STATUS_CHINESE_PUNCT),
        ("STATUS_TOOLBAR_VISIBLE", STATUS_TOOLBAR_VISIBLE),
        ("STATUS_MODE_CHANGED", STATUS_MODE_CHANGED),
        ("STATUS_CAPS_LOCK", STATUS_CAPS_LOCK),
        ("STATUS_HOST_RENDER_AVAIL", STATUS_HOST_RENDER_AVAIL),
        ("STATUS_SOFT_KEYBOARD", STATUS_SOFT_KEYBOARD),
        ("STATUS_SOFT_KEYBOARD_KEYS", STATUS_SOFT_KEYBOARD_KEYS),
        ("STATUS_HOTKEY_SESSION", STATUS_HOTKEY_SESSION),
    ]
    .into_iter()
    .collect();

    for (name, rv) in &rust {
        let cv = cpp
            .get(*name)
            .unwrap_or_else(|| panic!("C++ 侧没有 {name} —— 新增状态位时两侧都要加"));
        assert_eq!(
            rv, cv,
            "{name} 两侧位值不一致: Rust=0x{rv:04X} C++=0x{cv:04X}"
        );
    }
    // 反向：C++ 加了位而 Rust 没跟上，同样要红 —— 否则服务端永远不会置那一位。
    for name in cpp.keys() {
        assert!(
            rust.contains_key(name.as_str()),
            "C++ 侧有 {name} 而 Rust 侧没有 —— 新增状态位时两侧都要加"
        );
    }
}

/// 来源位不得与任何 `STATUS_*` 状态位重叠——两者共用 `CMD_SYSTEM_MODE_SWITCH`
/// 的同一个 u32。重叠的后果是静默的：来源编码会被读成「中文模式」等状态位。
#[test]
fn mode_switch_source_bits_do_not_collide_with_status_bits() {
    let status_bits = STATUS_CHINESE_MODE
        | STATUS_FULL_WIDTH
        | STATUS_CHINESE_PUNCT
        | STATUS_TOOLBAR_VISIBLE
        | STATUS_MODE_CHANGED
        | STATUS_CAPS_LOCK
        | STATUS_HOST_RENDER_AVAIL
        | STATUS_SOFT_KEYBOARD
        | STATUS_SOFT_KEYBOARD_KEYS
        | STATUS_HOTKEY_SESSION;
    assert_eq!(status_bits & MODE_SWITCH_SOURCE_MASK, 0);
    assert_eq!(MODE_SWITCH_SOURCE_MASK >> MODE_SWITCH_SOURCE_SHIFT, 0xF);
}

/// 编码必须与 `BinaryProtocol.h` 的 `ModeSwitchSource` 逐值对齐；
/// 旧 DLL 不带来源位（高 4 位为 0）时必须落到 `Unknown` 而不是某个真来源。
#[test]
fn mode_switch_source_decodes_wire_values() {
    let wire = |v: u32| ModeSwitchSource::from_flags(STATUS_CHINESE_MODE | (v << 28));
    assert_eq!(wire(0), ModeSwitchSource::Unknown);
    assert_eq!(wire(1), ModeSwitchSource::CompartmentOpenClose);
    assert_eq!(wire(2), ModeSwitchSource::CompartmentConversion);
    assert_eq!(wire(3), ModeSwitchSource::CtrlSpaceKey);
    assert_eq!(wire(4), ModeSwitchSource::Menu);
    assert_eq!(wire(0xF), ModeSwitchSource::Unknown); // 未知编码不 panic
    // 状态位本身不受来源位影响
    assert_ne!((STATUS_CHINESE_MODE | (3 << 28)) & STATUS_CHINESE_MODE, 0);
}

/// 头部与表项的实际内存布局必须等于两端约定的常量。
///
/// `repr(C, packed)` 下加字段不会报错，只会悄悄改变后续字段偏移，而 C++ 侧按固定偏移读。
#[test]
fn icon_shm_struct_sizes_match_declared() {
    assert_eq!(std::mem::size_of::<IconShmHeader>(), IconShmHeader::SIZE);
    assert_eq!(std::mem::size_of::<IconVariant>(), IconVariant::SIZE);
    assert_eq!(IconShmHeader::new().to_bytes().len(), 64);
}

/// 头部关键字段的字节偏移——C++ 侧按这些偏移读。
#[test]
fn icon_shm_header_field_offsets() {
    let mut h = IconShmHeader::new();
    h.sequence = 0xA1A2_A3A4;
    h.active_slot = 1;
    let b = h.to_bytes();
    assert_eq!(&b[0..4], &ICON_SHM_MAGIC.to_le_bytes()); // magic @0
    assert_eq!(&b[4..8], &ICON_SHM_VERSION.to_le_bytes()); // version @4
    assert_eq!(&b[8..12], &0xA1A2_A3A4u32.to_le_bytes()); // sequence @8
    assert_eq!(&b[12..16], &1u32.to_le_bytes()); // active_slot @12
    assert_eq!(&b[16..20], &(ICON_VARIANT_COUNT as u32).to_le_bytes()); // variant_count @16
    assert_eq!(&b[32..64], &[0u8; 32]); // reserved @32..64
}

/// 魔数字节序必须是可读的 'W','I','C','O'——十六进制转储时能一眼认出是图标 SHM。
#[test]
fn icon_shm_magic_reads_as_wico() {
    assert_eq!(&ICON_SHM_MAGIC.to_le_bytes(), b"WICO");
}

/// 布局自洽：表不越进 slot0，双 slot 不超出 SHM 总大小。
///
/// 加尺寸档时最容易越界，写穿到下一个 slot 表现为某些档位的图标随机变成别档的内容。
#[test]
fn icon_shm_layout_fits_without_overlap() {
    let table_end = ICON_TABLE_OFFSET + IconVariant::SIZE * ICON_VARIANT_COUNT;
    assert!(
        table_end <= ICON_SLOT0_OFFSET,
        "变体表 {table_end} 越进了 slot0 起点 {ICON_SLOT0_OFFSET}"
    );
    let needed = ICON_SLOT0_OFFSET + icon_slot_stride() * 2;
    assert!(
        needed <= ICON_SHM_SIZE,
        "双 slot 共需 {needed} 字节，超出 SHM 总大小 {ICON_SHM_SIZE}"
    );
}

/// 变体表覆盖全部 (尺寸 × 主题) 组合，且偏移首尾相接、无空洞无重叠。
#[test]
fn icon_variant_table_is_contiguous_and_complete() {
    let table = icon_variant_table();
    assert_eq!(table.len(), ICON_VARIANT_COUNT);

    let mut expect_offset = 0u32;
    for v in &table {
        assert_eq!({ v.offset }, expect_offset, "变体表出现空洞或重叠");
        assert_eq!({ v.byte_len }, icon_variant_bytes(v.size_px) as u32);
        expect_offset += v.byte_len;
    }
    // 表末偏移恰好等于单 slot 长度：既无剩余也无溢出
    assert_eq!(expect_offset as usize, icon_slot_stride());

    // 每个尺寸档都要有亮/暗两份
    for &size in &ICON_SIZES {
        for theme in [ICON_THEME_LIGHT, ICON_THEME_DARK] {
            assert!(
                table.iter().any(|v| v.size_px == size && v.theme == theme),
                "缺变体 size={size} theme={theme}"
            );
        }
    }
}

/// SHM 名的变体后缀走管道那套 `_dev`，不是应用目录那套 `Dev`。
/// 两种风格混用会让 dev 变体两端各开各的共享内存，握不上手且无任何报错。
/// C++ 侧 `Globals.h` 的 `WIND_ICON_SHM_NAME` 必须逐字一致。
#[test]
fn icon_shm_name_uses_pipe_style_suffix() {
    assert_eq!(icon_shm_name(""), "Local\\WindInput_IconShm_v1");
    assert_eq!(icon_shm_name("_dev"), "Local\\WindInput_IconShm_v1_dev");
}

/// 版本号进了名字，bump `ICON_SHM_VERSION` 会静默改掉 SHM 名，而 C++ 侧 `WIND_ICON_SHM_NAME`
/// 是写死的字面量；本测试让 bump 在这里先失败，逼着同步 `Globals.h`。
#[test]
fn icon_shm_name_carries_current_version() {
    assert_eq!(
        ICON_SHM_VERSION, 1,
        "改版本号必须同步 Globals.h 的 WIND_ICON_SHM_NAME"
    );
    assert!(
        icon_shm_name("").contains(&format!("_v{ICON_SHM_VERSION}")),
        "SHM 名必须带版本，否则改尺寸会被旧进程持有的同名 section 锁死"
    );
}

#[test]
fn shared_render_header_field_offsets_match_go_swift() {
    // 对齐 Swift SharedMemoryReader.swift / Go binary_protocol.go 的命名字段偏移
    let mut h = SharedRenderHeader::new(
        0x11223344, 0x55667788, 0x99AABBCC, 0xDDEE0011, 0x22334455, 0x66778899,
    );
    h.sequence = 0xA1A2A3A4;
    h.rect_count = 0xB1B2B3B4;
    h.rects_offset = 0xC1C2C3C4;
    h.rendered_hover_index = -3;
    h.target_instance_id = 0xE1E2E3E4;
    let b = h.to_bytes();
    assert_eq!(b.len(), 64);
    assert_eq!(SharedRenderHeader::SIZE, 64);
    assert_eq!(&b[8..12], &0xA1A2A3A4u32.to_le_bytes()); // sequence @8
    assert_eq!(&b[40..44], &0xB1B2B3B4u32.to_le_bytes()); // rect_count @40
    assert_eq!(&b[44..48], &0xC1C2C3C4u32.to_le_bytes()); // rects_offset @44
    assert_eq!(&b[48..52], &(-3i32).to_le_bytes()); // rendered_hover_index @48
    assert_eq!(&b[52..56], &0xE1E2E3E4u32.to_le_bytes()); // target_instance_id @52
    assert_eq!(&b[56..64], &[0u8; 8]); // reserved[2] @56..64 = 0
}

/// 构造一个焦点载荷字节串，`with_source` 决定是否带上第 39 字节。
fn focus_bytes(with_source: Option<u8>) -> Vec<u8> {
    let mut b = Vec::new();
    b.extend_from_slice(&111i32.to_le_bytes()); // caret.x
    b.extend_from_slice(&222i32.to_le_bytes()); // caret.y
    b.extend_from_slice(&33i32.to_le_bytes()); // caret.height
    b.extend_from_slice(&444i32.to_le_bytes()); // comp_start_x
    b.extend_from_slice(&555i32.to_le_bytes()); // comp_start_y
    b.extend_from_slice(&0x0123_4567_89AB_CDEFu64.to_le_bytes()); // client_token
    b.extend_from_slice(&0x8000_0000u64.to_le_bytes()); // input_scope_mask
    b.push(1); // disabled
    b.push(2); // reason
    if let Some(s) = with_source {
        b.push(s);
    }
    b
}

/// 尾部追加 `caret_source` 后，旧 DLL 的 38 字节包必须照常解析。
///
/// 长度判据若写成 `buf.len() < Self::SIZE`，每加一个尾部字段就会把所有旧 DLL 整体拒掉，
/// 表现为「切换应用后输入法毫无反应」，日志上看不出是解码拒绝还是没收到。
#[test]
fn focus_gained_v1_38_bytes_still_parses_with_unknown_source() {
    let b = focus_bytes(None);
    assert_eq!(b.len(), 38, "本用例要覆盖的就是旧 DLL 的 38 字节形态");
    let fg = FocusGainedPayload::from_bytes(&b).expect("38 字节旧包必须能解析");
    // CaretPayload 是 #[repr(packed)]，assert_eq! 对字段取引用会触发非对齐引用编译错误，
    // 先按值拷到局部再断言。
    let (cx, cy, ch) = (fg.caret.x, fg.caret.y, fg.caret.height);
    assert_eq!(cx, 111);
    assert_eq!(cy, 222);
    assert_eq!(ch, 33);
    assert_eq!(fg.client_token, 0x0123_4567_89AB_CDEF);
    assert_eq!(fg.disabled, 1);
    assert_eq!(fg.reason, 2);
    assert_eq!(
        fg.caret_source,
        caret_source::UNKNOWN,
        "旧包没有来源信息，必须落 UNKNOWN 而不是碰巧读到别的字节"
    );
}

/// 39 字节新包读出真实来源，且不影响任何既有字段的偏移。
#[test]
fn focus_gained_v2_39_bytes_reads_caret_source() {
    let b = focus_bytes(Some(caret_source::TSF_SELECTION as u8));
    assert_eq!(b.len(), 39);
    let fg = FocusGainedPayload::from_bytes(&b).expect("39 字节新包必须能解析");
    assert_eq!(fg.caret_source, caret_source::TSF_SELECTION);
    // 既有字段逐个复核：尾部追加的全部意义就是它们的偏移没动。
    // （packed 字段先按值拷到局部，理由同上。）
    let (cx, csx, csy) = (
        fg.caret.x,
        fg.caret.composition_start_x,
        fg.caret.composition_start_y,
    );
    assert_eq!(cx, 111);
    assert_eq!(csx, 444);
    assert_eq!(csy, 555);
    assert_eq!(fg.client_token, 0x0123_4567_89AB_CDEF);
    assert_eq!(fg.input_scope_mask, 0x8000_0000);
    assert_eq!(fg.disabled, 1);
    assert_eq!(fg.reason, 2);
    assert_eq!(FocusGainedPayload::SIZE, 39);
}

/// GUI 回退坐标不属于 TSF 语义域：焦点气泡的可信度闸门整个建立在这个判定上。
#[test]
fn gui_caret_is_not_tsf_domain() {
    assert!(caret_source::is_tsf(caret_source::TSF_SELECTION));
    assert!(caret_source::is_tsf(caret_source::TSF_COMPOSITION));
    assert!(caret_source::is_tsf(caret_source::TSF_CACHED));
    // 宿主经 IMM32 声明的候选位置：真插入点（组合起点下沿），与组合起点同源可比。
    assert!(caret_source::is_tsf(caret_source::IMM_CANDIDATE_FORM));
    // 以下每一个都曾经或可能冒充插入点，必须全部判否
    assert!(!caret_source::is_tsf(caret_source::GUI_CARET));
    assert!(!caret_source::is_tsf(caret_source::CONSOLE));
    assert!(!caret_source::is_tsf(caret_source::LAST_KNOWN));
    assert!(!caret_source::is_tsf(caret_source::UNKNOWN));
    assert!(!caret_source::is_tsf(caret_source::PRE_REFLOW));
    // TSF_DEFAULT_POS 出自 TSF context 却判否：宿主用退化矩形回答的正是「这里没有插入点」，
    // 整场组合恒为同一个屏幕角落，放进 TSF 域会被当权威坐标参与跟行与漂移校正。
    assert!(!caret_source::is_tsf(caret_source::TSF_DEFAULT_POS));
}

/// 来源名必须逐个有对应分支：`name()` 的 `_ => "unknown"` 兜底会把漏加的新来源静默显示成
/// `unknown`，日志上与「旧 DLL / macOS 短包」无从区分，而排查全靠日志里的 `src=`。
#[test]
fn every_caret_source_has_its_own_name() {
    let named = [
        (caret_source::TSF_SELECTION, "tsf_selection"),
        (caret_source::TSF_COMPOSITION, "tsf_composition"),
        (caret_source::TSF_CACHED, "tsf_cached"),
        (caret_source::GUI_CARET, "gui_caret"),
        (caret_source::CONSOLE, "console"),
        (caret_source::LAST_KNOWN, "last_known"),
        (caret_source::PRE_REFLOW, "pre_reflow"),
        (caret_source::TSF_DEFAULT_POS, "tsf_default_pos"),
        (caret_source::IMM_CANDIDATE_FORM, "imm_candidate_form"),
    ];
    for (src, want) in named {
        assert_eq!(
            caret_source::name(src),
            want,
            "来源 {src} 的名字不对——漏加分支时它会静默变成 unknown"
        );
    }
    assert_eq!(caret_source::name(caret_source::UNKNOWN), "unknown");
}

/// 坐标来源常量必须与 C++ 头文件逐值一致。
///
/// 与 [`status_bits_match_the_cpp_header`] 同一条理由，但后果更隐蔽：来源值错位时
/// `is_tsf` 会给出相反的答案，候选窗定位悄悄走错分支而日志一切正常。
#[test]
fn caret_sources_match_the_cpp_header() {
    let header = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../../wind_tsf/include/BinaryProtocol.h");
    let src = std::fs::read_to_string(&header)
        .unwrap_or_else(|e| panic!("读不到 {}: {e}", header.display()));

    // constexpr int32_t CARET_SRC_XXX = 7; —— 只认这一种形态，等号两侧空格随意。
    let mut cpp = std::collections::BTreeMap::new();
    for line in src.lines() {
        let line = line.trim();
        let Some(rest) = line.strip_prefix("constexpr int32_t CARET_SRC_") else {
            continue;
        };
        let Some((name, tail)) = rest.split_once('=') else {
            continue;
        };
        // 分号后多半跟着行尾注释（`= 0; // 旧协议 …`），先切掉再解析。
        let Some((val, _)) = tail.split_once(';') else {
            continue;
        };
        let Ok(v) = val.trim().parse::<i32>() else {
            continue;
        };
        cpp.insert(format!("CARET_SRC_{}", name.trim()), v);
    }
    assert!(
        cpp.len() >= 9,
        "只从头文件解析出 {} 个 CARET_SRC_* —— 多半是那边的写法变了，本测试已失效: {cpp:?}",
        cpp.len()
    );

    let rust: std::collections::BTreeMap<&str, i32> = [
        ("CARET_SRC_UNKNOWN", caret_source::UNKNOWN),
        ("CARET_SRC_TSF_SELECTION", caret_source::TSF_SELECTION),
        ("CARET_SRC_TSF_COMPOSITION", caret_source::TSF_COMPOSITION),
        ("CARET_SRC_TSF_CACHED", caret_source::TSF_CACHED),
        ("CARET_SRC_GUI_CARET", caret_source::GUI_CARET),
        ("CARET_SRC_CONSOLE", caret_source::CONSOLE),
        ("CARET_SRC_LAST_KNOWN", caret_source::LAST_KNOWN),
        ("CARET_SRC_PRE_REFLOW", caret_source::PRE_REFLOW),
        ("CARET_SRC_TSF_DEFAULT_POS", caret_source::TSF_DEFAULT_POS),
        (
            "CARET_SRC_IMM_CANDIDATE_FORM",
            caret_source::IMM_CANDIDATE_FORM,
        ),
    ]
    .into_iter()
    .collect();

    for (name, rv) in &rust {
        let cv = cpp
            .get(*name)
            .unwrap_or_else(|| panic!("C++ 侧没有 {name} —— 新增坐标来源时两侧都要加"));
        assert_eq!(rv, cv, "{name} 两侧取值不一致: Rust={rv} C++={cv}");
    }
    // 反向：C++ 加了来源而 Rust 没跟上，同样要红 —— 否则那一类坐标会落进
    // `name()` 的 unknown 兜底，而 `is_tsf` 会按「非 TSF」静默处理它。
    for name in cpp.keys() {
        assert!(
            rust.contains_key(name.as_str()),
            "C++ 侧有 {name} 而 Rust 侧没有 —— 新增坐标来源时两侧都要加"
        );
    }
}
