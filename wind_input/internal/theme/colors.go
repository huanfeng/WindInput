package theme

import (
	"fmt"
	"image/color"
	"strconv"
	"strings"
)

// ParseHexColor parses a hex color string (#RRGGBB or #RRGGBBAA) to color.Color
func ParseHexColor(hex string) (color.Color, error) {
	hex = strings.TrimPrefix(hex, "#")

	switch len(hex) {
	case 6: // RRGGBB
		r, err := strconv.ParseUint(hex[0:2], 16, 8)
		if err != nil {
			return nil, fmt.Errorf("invalid red component: %w", err)
		}
		g, err := strconv.ParseUint(hex[2:4], 16, 8)
		if err != nil {
			return nil, fmt.Errorf("invalid green component: %w", err)
		}
		b, err := strconv.ParseUint(hex[4:6], 16, 8)
		if err != nil {
			return nil, fmt.Errorf("invalid blue component: %w", err)
		}
		return color.RGBA{R: uint8(r), G: uint8(g), B: uint8(b), A: 255}, nil

	case 8: // RRGGBBAA
		r, err := strconv.ParseUint(hex[0:2], 16, 8)
		if err != nil {
			return nil, fmt.Errorf("invalid red component: %w", err)
		}
		g, err := strconv.ParseUint(hex[2:4], 16, 8)
		if err != nil {
			return nil, fmt.Errorf("invalid green component: %w", err)
		}
		b, err := strconv.ParseUint(hex[4:6], 16, 8)
		if err != nil {
			return nil, fmt.Errorf("invalid blue component: %w", err)
		}
		a, err := strconv.ParseUint(hex[6:8], 16, 8)
		if err != nil {
			return nil, fmt.Errorf("invalid alpha component: %w", err)
		}
		return color.RGBA{R: uint8(r), G: uint8(g), B: uint8(b), A: uint8(a)}, nil

	default:
		return nil, fmt.Errorf("invalid hex color format: expected 6 or 8 characters, got %d", len(hex))
	}
}

// MustParseHexColor parses a hex color string, returning the default if parsing fails
func MustParseHexColor(hex string, defaultColor color.Color) color.Color {
	if hex == "" {
		return defaultColor
	}
	c, err := ParseHexColor(hex)
	if err != nil {
		return defaultColor
	}
	return c
}

// ColorToHex converts a color.Color to a hex string (#RRGGBBAA)
func ColorToHex(c color.Color) string {
	if c == nil {
		return "#00000000"
	}
	r, g, b, a := c.RGBA()
	// RGBA() returns values in the range [0, 65535], convert to [0, 255]
	return fmt.Sprintf("#%02X%02X%02X%02X", r>>8, g>>8, b>>8, a>>8)
}

// ColorToHexRGB converts a color.Color to a hex string without alpha (#RRGGBB)
func ColorToHexRGB(c color.Color) string {
	if c == nil {
		return "#000000"
	}
	r, g, b, _ := c.RGBA()
	return fmt.Sprintf("#%02X%02X%02X", r>>8, g>>8, b>>8)
}

// ColorToRGBA converts a color.Color to RGBA float values (0.0-1.0) for gg library
func ColorToRGBA(c color.Color) (r, g, b, a float64) {
	if c == nil {
		return 0, 0, 0, 0
	}
	ri, gi, bi, ai := c.RGBA()
	return float64(ri) / 65535.0, float64(gi) / 65535.0, float64(bi) / 65535.0, float64(ai) / 65535.0
}

// ColorToRGB converts a color.Color to RGB float values (0.0-1.0) for gg library
func ColorToRGB(c color.Color) (r, g, b float64) {
	if c == nil {
		return 0, 0, 0
	}
	ri, gi, bi, _ := c.RGBA()
	return float64(ri) / 65535.0, float64(gi) / 65535.0, float64(bi) / 65535.0
}
