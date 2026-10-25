# 纯 C++ 单测的兜底构建（不需要 cmake、不需要 Fcitx5）。addon 本体走 CMakeLists.txt。
#
#   make -C wind_linux test          # 编译并运行全部单测
#   make -C wind_linux clean
#
# 产物落在 $(BUILD)（默认 wind_linux/build/tests，已 gitignore）。

CXX ?= g++
BUILD ?= build/tests
CXXFLAGS ?= -std=c++17 -O1 -g -Wall -Wextra -Werror -pthread
CPPFLAGS += -Iinclude -DWIND_PROTOCOL_RS=\"$(abspath ../wind_input/crates/wind-ipc/src/protocol.rs)\" \
	-DWIND_REPO_DIR=\"$(abspath ..)\"

CORE_SRC := $(wildcard src/core/*.cpp)
TESTS := codec_test keymap_test router_test bridge_test protocol_sync_test shm_test launcher_test settings_launcher_test menu_test host_ui_test overlay_input_test keysynth_test gnome_caret_geom_test

.PHONY: test clean
test: $(addprefix $(BUILD)/,$(TESTS))
	@fail=0; for t in $(TESTS); do \
		echo "=== $$t"; \
		$(BUILD)/$$t || fail=1; \
	done; \
	if [ $$fail -eq 0 ]; then echo "单测全部通过"; else echo "单测有失败"; exit 1; fi

$(BUILD)/%: tests/%.cpp $(CORE_SRC) $(wildcard include/*.h) tests/TestHarness.h
	@mkdir -p $(BUILD)
	$(CXX) $(CXXFLAGS) $(CPPFLAGS) -o $@ $< $(CORE_SRC)

clean:
	rm -rf $(BUILD)
