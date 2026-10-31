# 词库数据目录（等价于 scripts/dev.sh 的 gen-data + assemble_data）。
#
# 输入：
#   - 本仓 data/（方案、主题、配置等源文件）+ scripts/lib/gen-compat.sh（生成 Linux 版
#     compat.toml）+ wind_input/apps/wind-tools/data/（gen_dict 的配置目录）
#   - 上游词库源：固定 commit 的 FOD（见 upstream），只取构建真正用到的文件
#   - wind-tools 的生成工具：gen_dict / gen_opencc / gen_pinyin / gen_unigram / gen_aux_code
#
# 输出：$out 就是运行时 data/ 目录本身（schemas/、config.toml、opencc/ …），
# 由 service.nix 复制到 `<exe 同级>/data/`。
{
  lib,
  stdenv,
  stdenvNoCC,
  rustPlatform,
  curl,
  cacert,
  version,
}:
let
  # 派生输入只留真正被读到的三处。整仓当 src 时，改任何一个文件（哪怕 README.md）都会改掉
  # 本派生的输入哈希，级联重建 data → service → fcitx5 → 汇总包四层：
  #   data/                             入库的方案 / 主题 / 配置（buildPhase 第 1 步整份复制）
  #   scripts/lib/gen-compat.sh         生成 Linux 版 compat.toml（第 9 步）
  #   wind_input/apps/wind-tools/data/  gen_dict 的配置目录：custom_*.txt / boost_words.txt
  #                                     都相对它解析（gen_dict/config.rs::resolve_paths）
  #   wind_input/data/                  dev.sh 里「存在就合并」的可选源（第 1b 段）
  # ⚠ wind_input/ 的其它改动仍会经下面的 tools 派生重建数据（gen_* 链接 workspace crate）；
  #   这里切断的是 data/、文档、wind_linux/ 等无关文件的耦合。
  # tools 派生要整棵 wind_input/（cargo workspace），但用 `src-filter.nix` 剔掉 `target/`：
  # 本机 cargo 产物不是源码，进派生输入会让 `path:` 下每轮构建都换一个哈希。
  root = ../.;
  src = lib.cleanSourceWith {
    name = "windinput-data-context";
    src = root;
    filter =
      path: _type:
      let
        rel = lib.removePrefix "${toString root}/" (toString path);
        keep =
          prefix: rel == prefix || lib.hasPrefix "${prefix}/" rel || lib.hasPrefix "${rel}/" prefix;
      in
      rel == "" || lib.any keep [
        "data"
        "scripts/lib/gen-compat.sh"
        "wind_input/apps/wind-tools/data"
        "wind_input/data"          # 第 1b 段合并 settings（当前无此目录，留着防静默缺失）
      ];
  };
  ws = ../wind_input;
  srcFilter = import ./src-filter.nix { inherit lib; };

  # ── 上游词库源 ──────────────────────────────────────────────────────────
  # 单一固定输出派生（递归哈希）：一次性下载所有需要的文件，避免把 rime-frost /
  # CLDR 等整仓拖进 store（CLDR 只用于重新生成 emoji 名表，本构建不需要）。
  # 版本钉在 commit 上，上游分支移动不会改变产物。
  frostRev = "7ced7f8139d6fd11a1a15d6c1412f5c14ca0da3f";
  pinyinRev = "9d05c9e29cb6a7e5c79f2f5279f5c1014246a19a";
  openccRev = "a806b775d038f9fa7c061f50d270115e7ad7da5b";
  wubiRev = "18a6016c3d08a20f713f8f8e1791a2e79e0be081";
  auxRev = "f9780033396f8998697c2a004d40bd543c46308b";
  strokeRev = "1e8fff9b9494ddec23b0cbc526bcfd8171a6fd48";
  emojiRev = "39d232996de79a95d7bf4b5e8533feed5326b075";
  hanziRev = "6970aa7e4030253e22accfa5297d7e292d75a597";

  raw = repo: rev: "https://raw.githubusercontent.com/${repo}/${rev}";

  frostCells = [
    "exthot"
    "idiom"
    "place"
    "history"
    "name"
    "name2"
    "sport"
    "geography"
    "animal"
    "computer"
    "music"
    "media"
    "industry_product"
    "inputmethod"
    "chess"
    "chess2"
  ];

  upstream = stdenvNoCC.mkDerivation {
    pname = "windinput-upstream-srcs";
    inherit version;
    dontUnpack = true;
    nativeBuildInputs = [
      curl
      cacert
    ];
    SSL_CERT_FILE = "${cacert}/etc/ssl/certs/ca-bundle.crt";

    buildCommand = ''
      set -euo pipefail
      mkdir -p $out

      get() {
        mkdir -p "$(dirname "$2")"
        curl -fsSL --retry 3 --retry-delay 2 -o "$2" "$1"
      }

      # rime-frost：拼音主库 + 单字/扩展表 + 细胞词库 + 英文 + emoji/符号表
      frost=${raw "gaboolic/rime-frost" frostRev}
      get "$frost/rime_frost.dict.yaml" "$out/rime-frost/rime_frost.dict.yaml"
      for f in 8105 41448 base ext others corrections tencent; do
        get "$frost/cn_dicts/$f.dict.yaml" "$out/rime-frost/cn_dicts/$f.dict.yaml"
      done
      ${lib.concatMapStringsSep "\n" (c: ''
        get "$frost/cn_dicts_cell/${c}.dict.yaml" "$out/rime-frost/cn_dicts_cell/${c}.dict.yaml"
      '') frostCells}
      for f in en en_ext; do
        get "$frost/en_dicts/$f.dict.yaml" "$out/rime-frost/en_dicts/$f.dict.yaml"
      done
      for f in emoji others; do
        get "$frost/opencc/$f.txt" "$out/rime-frost/opencc/$f.txt"
      done

      # pinyin-data：汉字拼音反查表源
      pinyin=${raw "mozillazg/pinyin-data" pinyinRev}
      for f in pinyin kXHC1983 kTGHZ2013 kMandarin_8105 overwrite; do
        get "$pinyin/$f.txt" "$out/pinyin-data/$f.txt"
      done

      # OpenCC：简繁/繁简/台湾/香港词典
      opencc=${raw "BYVoid/OpenCC" openccRev}/data/dictionary
      for f in STCharacters STPhrases TSCharacters TSPhrases TWVariants TWPhrases HKVariants; do
        get "$opencc/$f.txt" "$out/opencc/dictionaries/$f.txt"
      done

      # rime-wubi86-jidian：五笔主库 / 扩展 / 行政区域
      wubi=${raw "KyleBing/rime-wubi86-jidian" wubiRev}
      for f in wubi86_jidian wubi86_jidian_extra wubi86_jidian_extra_district; do
        get "$wubi/$f.dict.yaml" "$out/rime-wubi/$f.dict.yaml"
      done

      # 辅助码：小鹤 / 自然码 + 笔画上游全表 + 字集
      aux=${raw "HowcanoeWang/rime-lua-aux-code" auxRev}/aux_code
      get "$aux/flypy_full.txt" "$out/aux-code/flypy_full.txt"
      get "$aux/ZRM-wanxiang.txt" "$out/aux-code/ZRM-wanxiang.txt"
      get "${raw "rime/rime-stroke" strokeRev}/stroke.dict.yaml" "$out/aux-code/stroke.dict.yaml"
      hanzi=${raw "zispace/hanzi-chars" hanziRev}
      get "$hanzi/data-charset/GB%2018030-2000.txt" "$out/aux-code/charset/GB 18030-2000.txt"
      get "$hanzi/data-charlist/%E3%80%8A%E9%80%9A%E7%94%A8%E8%A7%84%E8%8C%83%E6%B1%89%E5%AD%97%E8%A1%A8%E3%80%8B%EF%BC%882013%E5%B9%B4%EF%BC%89.txt" \
          "$out/aux-code/charset/《通用规范汉字表》（2013年）.txt"
      get "$hanzi/data-unicode/Unicode-CJK%20%E3%80%87.txt" "$out/aux-code/charset/Unicode-CJK 〇.txt"

      # rime-emoji：候选扩展表 + 许可证全文（分发合规要求）
      emoji=${raw "rime/rime-emoji" emojiRev}
      for f in emoji_word emoji_category; do
        get "$emoji/opencc/$f.txt" "$out/rime-emoji/opencc/$f.txt"
      done
      get "$emoji/LICENSE" "$out/rime-emoji/LICENSE"
    '';

    outputHashMode = "recursive";
    outputHash = "sha256-bP+7yUBmfjh8qz3hW+rSbD6xmfIMrH5+B3J4eKqMTzM=";
  };

  # ── 生成工具 ────────────────────────────────────────────────────────────
  tools = rustPlatform.buildRustPackage {
    pname = "windinput-tools";
    inherit version;
    # 只剔 `target/`（见 `src-filter.nix`）：本机 cargo 产物不是源码，进派生输入会让
    # `path:` 下每轮构建都换 store 路径，级联重建四层。
    src = srcFilter.withoutBuildOutput ws;
    cargoLock.lockFile = ../wind_input/Cargo.lock;
    cargoBuildFlags = [ "-p" "wind-tools" ];
    doCheck = false;

    installPhase = ''
      runHook preInstall
      mkdir -p $out/bin
      for b in gen_dict gen_opencc gen_pinyin gen_unigram gen_aux_code; do
        bin=$(find target -type f -name "$b" -path '*/release/*' -print -quit)
        [ -n "$bin" ] || { echo "找不到 $b 构建产物" >&2; exit 1; }
        install -Dm755 "$bin" "$out/bin/$b"
      done
      runHook postInstall
    '';
  };
in
stdenv.mkDerivation {
  pname = "windinput-data";
  inherit version src;

  nativeBuildInputs = [ tools ];

  # 这些生成物不入库：缺了 gen-data 时安装器会发残包，故在构建期直接断言关键文件。
  buildPhase = ''
    runHook preBuild
    set -euo pipefail

    # 源码树在 store 里是只读的，所有中间产物放到构建目录。
    work=$NIX_BUILD_TOP/work
    rm -rf "$work"
    mkdir -p "$work"
    cache=$work/cache
    cp -r ${upstream} "$cache"
    chmod -R u+w "$cache"

    data=$work/data
    mkdir -p "$data"

    # 1. 入库的源文件（方案 / 主题 / 配置）。store 里的目录是 0555，复制后要放开写权限。
    cp -r ${src}/data/. "$data"/
    chmod -R u+w "$data"
    # AI 工具（Claude Code / oh-my-claudecode 等）在会话 cwd 落的状态目录，不随包分发。
    # 判据与 dev.sh 的 strip_tool_state 一致：任意深度的同名目录，不只是顶层。
    for d in .omc .omx .claude .remember; do
      find "$data" -type d -name "$d" -prune -exec rm -rf {} + 2>/dev/null || true
    done

    # 1b. 与 dev.sh 同构的守卫：manifest.toml 已在 c52be632 退役（能力清单改由
    #     wind-rpc 的 system.capabilities 动态生成），仓库里当前没有 wind_input/data/，
    #     故这一段暂不执行；白名单仍带上 wind_input/data，让它若回归时 dev/deb 与 Nix 两条
    #     链的产物一致（否则 cp 静默缺席，断言列表里也没有它，排查成本高）。
    if [ -d ${src}/wind_input/data/settings ]; then
      mkdir -p "$data/settings"
      cp -r ${src}/wind_input/data/settings/. "$data/settings/"
      chmod -R u+w "$data/settings"
    fi

    # 2. rime-frost 拼音词库
    pinyin=$data/schemas/pinyin
    mkdir -p "$pinyin/cn_dicts" "$pinyin/ext"
    cp -f "$cache/rime-frost/rime_frost.dict.yaml" "$pinyin/"
    for f in 8105 41448 base ext others corrections; do
      cp -f "$cache/rime-frost/cn_dicts/$f.dict.yaml" "$pinyin/cn_dicts/"
    done
    for f in ${lib.concatStringsSep " " frostCells}; do
      cp -f "$cache/rime-frost/cn_dicts_cell/$f.dict.yaml" "$pinyin/ext/"
    done
    awk -v dir="$pinyin" -f ${./strip-unshipped-imports.awk} \
      "$pinyin/rime_frost.dict.yaml" > "$pinyin/rime_frost.dict.yaml.tmp"
    mv -f "$pinyin/rime_frost.dict.yaml.tmp" "$pinyin/rime_frost.dict.yaml"

    # 3. 英文词库
    mkdir -p "$data/schemas/english"
    for f in en en_ext; do
      cp -f "$cache/rime-frost/en_dicts/$f.dict.yaml" "$data/schemas/english/"
    done

    # 4. unigram（仅 gen_dict 给五笔扩展库赋权用，不随 data 分发）
    mkdir -p "$cache/pinyin-frost"
    gen_unigram --rime "$cache/rime-frost/cn_dicts" --out "$cache/pinyin-frost/unigram.txt"

    # 4b. 汉字拼音反查表
    gen_pinyin --src "$cache/pinyin-data" --out "$data/pinyin_map.txt"

    # 5. OpenCC → .octrie
    mkdir -p "$data/opencc"
    gen_opencc --src "$cache/opencc/dictionaries" --out "$data/opencc"

    # 6. 五笔词库（主库重排 + extra 拆 4 库 + district 透传）。
    #    gen_dict 的默认配置路径是编译期 CARGO_MANIFEST_DIR（Nix 里已不存在），必须显式给。
    mkdir -p "$data/schemas/wubi86"
    # --version-date 是词库头部 `version:` 的版本戳（gen_dict 的默认值取当日 UTC，会让产物
    # 日日漂移）。这里传产品版本，语义是「由哪个版本生成」，不是日期 —— wind-dict 只解析
    # `sort:` / `columns:`，`version:` 无消费者，故与 deb 链（不传，取当日 UTC）的差异无影响。
    gen_dict \
      --cache "$cache" \
      --out "$data/schemas/wubi86" \
      --report "$cache/rime-wubi" \
      --version-date ${version} \
      --config ${src}/wind_input/apps/wind-tools/data/gen_dict/gen_dict.toml

    # 7. 辅助码表 + 笔画方案词库
    mkdir -p "$data/schemas/aux_code"
    gen_aux_code --cache "$cache" --out "$data/schemas/aux_code" --schema-out "$data/schemas"

    # 8. Emoji 候选扩展表（原样复制，不做构建期转换）
    mkdir -p "$data/emoji"
    cp -f "$cache/rime-emoji/opencc/emoji_word.txt" "$data/emoji/"
    cp -f "$cache/rime-emoji/opencc/emoji_category.txt" "$data/emoji/"
    cp -f "$cache/rime-emoji/LICENSE" "$data/emoji/LICENSE"

    # 8b. rime-frost 的 emoji / 符号表
    cp -f "$cache/rime-frost/opencc/emoji.txt" "$data/emoji/frost_emoji.txt"
    awk -v excl="著名表演艺术家" -f ${./strip-frost-sections.awk} \
      "$cache/rime-frost/opencc/others.txt" > "$data/emoji/frost_others.txt"

    # 9. Linux 版应用兼容规则（Windows 规则不能随包带）
    bash ${src}/scripts/lib/gen-compat.sh linux \
      "$data/compat.toml" "$data/compat.toml.linux"
    mv -f "$data/compat.toml.linux" "$data/compat.toml"

    # 10. 发布级断言：关键运行时数据缺失即失败，不产出残缺数据目录
    for f in \
      "$data/pinyin_map.txt" \
      "$data/schemas/pinyin/cn_dicts/base.dict.yaml" \
      "$data/schemas/wubi86/wubi86_jidian.dict.yaml" \
      "$data/schemas/wubi86/wubi86_jidian_extra.dict.yaml" \
      "$data/schemas/stroke/stroke.dict.yaml" \
      "$data/opencc/STCharacters.octrie" \
      "$data/emoji/emoji_word.txt" \
      "$data/emoji/LICENSE"
    do
      [ -s "$f" ] || { echo "数据组装不完整：缺 $f" >&2; exit 1; }
    done

    runHook postBuild
  '';

  installPhase = ''
    runHook preInstall
    mkdir -p $out
    cp -a "$NIX_BUILD_TOP/work/data/." $out/
    runHook postInstall
  '';
}
