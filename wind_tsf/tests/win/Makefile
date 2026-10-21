# wind_tsf Windows 目标集成测试（链接真实 IPCClient.cpp，要 Windows 运行时）
#
# 与 ../*_test.cpp（纯 C++、g++ 本机跑）分开放：这里的用例要命名管道等 Win32 运行时，
# 交叉编成 x64 exe 后用 wine 跑，或拷到 Windows 上直接跑。工具链同 ../../Makefile。
#
#   make -C wind_tsf/tests/win          # → build/ipc_reconnect_test.exe
#   make -C wind_tsf/tests/win run      # wine 执行
#   make -C wind_tsf/tests/win IPC_SRC=/path/IPCClient.cpp TSF_INC=/path/include EXTRA=-DIPC_TEST_LEGACY
#                                       # 对旧版代码复现

CLANG     ?= clang++-19
XWIN_ROOT ?= $(HOME)/.cache/cargo-xwin/xwin
WINE      ?= wine

TSF_INC ?= ../../include
IPC_SRC ?= ../../src/IPCClient.cpp
OUTDIR  ?= build
EXTRA   ?=

TRIPLE  := x86_64-pc-windows-msvc
INCFLAGS := -isystem $(XWIN_ROOT)/crt/include \
            -isystem $(XWIN_ROOT)/sdk/include/um \
            -isystem $(XWIN_ROOT)/sdk/include/shared \
            -isystem $(XWIN_ROOT)/sdk/include/ucrt
LIBFLAGS := -Xlinker /LIBPATH:$(XWIN_ROOT)/crt/lib/x86_64 \
            -Xlinker /LIBPATH:$(XWIN_ROOT)/sdk/lib/um/x86_64 \
            -Xlinker /LIBPATH:$(XWIN_ROOT)/sdk/lib/ucrt/x86_64

CXXFLAGS := --target=$(TRIPLE) -fms-runtime-lib=static -std=c++17 -O2 -Wall -Wextra -Wno-unused-parameter \
  -DUNICODE -D_UNICODE -D_WIN32_WINNT=0x0A00 -DWINVER=0x0A00 -DNTDDI_VERSION=0x0A000000 \
  -I$(TSF_INC) $(INCFLAGS) $(EXTRA)
LDFLAGS := --target=$(TRIPLE) -fms-runtime-lib=static -fuse-ld=lld $(LIBFLAGS)
LIBS := -ladvapi32 -luser32 -lkernel32

EXE := $(OUTDIR)/ipc_reconnect_test.exe

.PHONY: all run clean
all: $(EXE)

$(EXE): ipc_reconnect_test.cpp $(IPC_SRC) $(wildcard $(TSF_INC)/*.h)
	@mkdir -p $(OUTDIR)
	$(CLANG) $(CXXFLAGS) $(LDFLAGS) -o $@ ipc_reconnect_test.cpp $(IPC_SRC) $(LIBS)

run: $(EXE)
	WINEDEBUG=-all $(WINE) $(EXE)

clean:
	rm -rf $(OUTDIR)
