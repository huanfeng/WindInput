package theme

import (
	"image/color"
	"testing"
)

func TestParseHexColor(t *testing.T) {
	tests := []struct {
		name    string
		hex     string
		want    color.RGBA
		wantErr bool
	}{
		{
			name: "6-digit hex",
			hex:  "#FF0000",
			want: color.RGBA{R: 255, G: 0, B: 0, A: 255},
		},
		{
			name: "6-digit hex lowercase",
			hex:  "#00ff00",
			want: color.RGBA{R: 0, G: 255, B: 0, A: 255},
		},
		{
			name: "8-digit hex with alpha",
			hex:  "#FF0000F0",
			want: color.RGBA{R: 255, G: 0, B: 0, A: 240},
		},
		{
			name: "8-digit hex transparent",
			hex:  "#00000000",
			want: color.RGBA{R: 0, G: 0, B: 0, A: 0},
		},
		{
			name: "without hash",
			hex:  "4285F4",
			want: color.RGBA{R: 66, G: 133, B: 244, A: 255},
		},
		{
			name:    "invalid length",
			hex:     "#FFF",
			wantErr: true,
		},
		{
			name:    "invalid characters",
			hex:     "#GGGGGG",
			wantErr: true,
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			got, err := ParseHexColor(tt.hex)
			if (err != nil) != tt.wantErr {
				t.Errorf("ParseHexColor() error = %v, wantErr %v", err, tt.wantErr)
				return
			}
			if !tt.wantErr {
				gotRGBA, ok := got.(color.RGBA)
				if !ok {
					t.Errorf("ParseHexColor() returned non-RGBA color")
					return
				}
				if gotRGBA != tt.want {
					t.Errorf("ParseHexColor() = %v, want %v", gotRGBA, tt.want)
				}
			}
		})
	}
}

func TestMustParseHexColor(t *testing.T) {
	defaultColor := color.RGBA{R: 128, G: 128, B: 128, A: 255}

	tests := []struct {
		name string
		hex  string
		want color.RGBA
	}{
		{
			name: "valid color",
			hex:  "#FF0000",
			want: color.RGBA{R: 255, G: 0, B: 0, A: 255},
		},
		{
			name: "empty string uses default",
			hex:  "",
			want: defaultColor,
		},
		{
			name: "invalid color uses default",
			hex:  "#invalid",
			want: defaultColor,
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			got := MustParseHexColor(tt.hex, defaultColor)
			gotRGBA, ok := got.(color.RGBA)
			if !ok {
				t.Errorf("MustParseHexColor() returned non-RGBA color")
				return
			}
			if gotRGBA != tt.want {
				t.Errorf("MustParseHexColor() = %v, want %v", gotRGBA, tt.want)
			}
		})
	}
}

func TestColorToHex(t *testing.T) {
	tests := []struct {
		name  string
		color color.Color
		want  string
	}{
		{
			name:  "red opaque",
			color: color.RGBA{R: 255, G: 0, B: 0, A: 255},
			want:  "#FF0000FF",
		},
		{
			name:  "transparent",
			color: color.RGBA{R: 0, G: 0, B: 0, A: 0},
			want:  "#00000000",
		},
		{
			name:  "semi-transparent blue",
			color: color.RGBA{R: 66, G: 133, B: 244, A: 128},
			want:  "#4285F480",
		},
		{
			name:  "nil color",
			color: nil,
			want:  "#00000000",
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			got := ColorToHex(tt.color)
			if got != tt.want {
				t.Errorf("ColorToHex() = %v, want %v", got, tt.want)
			}
		})
	}
}

func TestColorToRGBA(t *testing.T) {
	c := color.RGBA{R: 255, G: 128, B: 0, A: 255}
	r, g, b, a := ColorToRGBA(c)

	if r < 0.99 || r > 1.01 {
		t.Errorf("ColorToRGBA() r = %v, want ~1.0", r)
	}
	if g < 0.49 || g > 0.51 {
		t.Errorf("ColorToRGBA() g = %v, want ~0.5", g)
	}
	if b != 0 {
		t.Errorf("ColorToRGBA() b = %v, want 0", b)
	}
	if a < 0.99 || a > 1.01 {
		t.Errorf("ColorToRGBA() a = %v, want ~1.0", a)
	}
}
