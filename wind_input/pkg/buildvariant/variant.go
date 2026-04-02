package buildvariant

// variant 通过 ldflags 注入: -X github.com/huanfeng/wind_input/pkg/buildvariant.variant=debug
var variant = ""

func IsDebug() bool {
	return variant == "debug"
}

func Suffix() string {
	if variant == "debug" {
		return "_debug"
	}
	return ""
}

func AppName() string {
	if variant == "debug" {
		return "WindInputDebug"
	}
	return "WindInput"
}

func DisplayName() string {
	if variant == "debug" {
		return "清风输入法 (Debug)"
	}
	return "清风输入法"
}
