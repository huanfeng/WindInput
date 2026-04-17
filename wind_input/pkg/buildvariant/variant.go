package buildvariant

// variant 通过 ldflags 注入: -X github.com/huanfeng/wind_input/pkg/buildvariant.variant=debug
var variant = ""

const (
	AppNameRelease = "WindInput"
	AppNameDebug   = "WindInputDebug"
)

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
		return AppNameDebug
	}
	return AppNameRelease
}

func DisplayName() string {
	if variant == "debug" {
		return "清风输入法 (Debug)"
	}
	return "清风输入法"
}
