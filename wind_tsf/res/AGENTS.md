<!-- Parent: ../AGENTS.md -->
<!-- Generated: 2026-03-11 | Updated: 2026-03-11 -->

# res/ - Resource Files

## Purpose

Resource files for the TSF DLL, including icon, version information, and manifest. These files are compiled into the DLL binary at build time via the resource compiler (RC.exe).

## Key Files

| File | Description |
|------|-------------|
| `resource.rc` | Resource script defining icon and other resources |
| `wind_input.ico` | Icon image displayed in Windows language bar and settings |

## Resource Content

### resource.rc

```rc
// WindInput TSF Resource File
// Icon resource for input method display in Windows settings

// Icon index 0 - Main input method icon
0 ICON "wind_input.ico"
```

**Purpose:**
- Defines a single icon resource (index 0)
- Icon is displayed in:
  - Windows language bar when 清风输入法 is active
  - Windows Settings > Time & Language > Input Method list
  - IME selector dropdown

**Resource IDs:**
- `0` - Main icon for input method

### wind_input.ico

**Format:** Standard Windows ICO file (multiple resolutions)

**Required Sizes:**
- 16x16 (language bar)
- 32x32 (settings, larger context)
- 48x48 (high DPI support)
- 256x256 (Windows 11 modern UI)

**Best Practices:**
- Use clear, recognizable visual design (Chinese character or windmill motif)
- Provide high-contrast color palette for accessibility
- Test icon visibility on taskbar at all DPI scales (96, 120, 144, 192 DPI)

## For AI Agents

### Working In This Directory

When modifying resources:

1. **Icon format** - Use standard ICO file with PNG-compressed data for modern Windows
2. **Multiple resolutions** - Include 16x16, 32x32, 48x48, and 256x256 in single .ico file
3. **High DPI** - Test icons at 150% (144 DPI) and 200% (192 DPI) scaling
4. **Transparency** - Use alpha channel for smooth anti-aliasing and transparency
5. **Color depth** - 32-bit RGBA recommended for best quality and transparency support

### Adding Resources

To add new resources to resource.rc:

```rc
// String resources
STRINGTABLE
BEGIN
    IDS_APP_NAME,           L"清风输入法"
    IDS_APP_DESCRIPTION,    L"Windows TSF Chinese Input Method"
END

// Dialog resource (if needed)
IDD_ABOUT DIALOGEX 0, 0, 256, 200, WS_POPUP | WS_CAPTION
STYLE DS_SHELLFONT
CAPTION "About 清风输入法"
FONT 9, "MS Shell Dlg 2"
BEGIN
    DEFPUSHBUTTON   "OK", IDOK, 100, 180, 50, 14
    LTEXT           "Version 1.0", IDC_STATIC, 10, 10, 100, 14
END
```

### Resource Compiler Flags

In CMakeLists.txt, RC.exe is invoked with:
```bash
rc.exe /d UNICODE /d _UNICODE /fo resource.res resource.rc
```

**Flags:**
- `/d UNICODE` - Enable Unicode support
- `/d _UNICODE` - Define _UNICODE macro
- `/fo` - Output filename (resource.res)

### Testing Resources

**Verify Icon Display:**
1. Build DLL: `cmake --build . --config Release`
2. Register: `regsvr32 wind_tsf.dll`
3. Open Windows Settings > Time & Language > Input Method
4. Check 清风输入法 appears with correct icon
5. Switch to input method
6. Verify language bar shows icon at 16x16
7. Test at different DPI scales (150%, 200%)

**Verify No Resource Errors:**
```bash
# After build, check for resource warnings
dumpbin /resources wind_tsf.dll | findstr "Icon"
```

## Dependencies

### Internal
- `resource.rc` includes `wind_input.ico` via ICON statement

### External
- Windows Resource Compiler (RC.exe) - part of Windows SDK
- Icon editor tools (not part of build, used offline):
  - GIMP
  - Paint.NET
  - Photoshop
  - Icon Editor (Visual Studio built-in)

## Common Tasks

### Creating High-Quality Icons

1. **Design in 256x256** as primary
2. **Export multiple sizes:**
   - 256x256 (primary)
   - 128x128 (high DPI fallback)
   - 64x64 (explorer view)
   - 48x48 (settings, medium)
   - 32x32 (settings, small, language bar double-size)
   - 16x16 (language bar actual)
3. **Apply anti-aliasing** at each size
4. **Keep transparency** via alpha channel
5. **Use ICO format** - package all sizes in single .ico file

### Adding Version Information

To add version info to the DLL, extend resource.rc:

```rc
1 VERSIONINFO
FILEVERSION 1,0,0,0
PRODUCTVERSION 1,0,0,0
FILEOS VOS__WINDOWS32
FILETYPE VFT_DLL
BEGIN
    BLOCK "StringFileInfo"
    BEGIN
        BLOCK "080404B0"
        BEGIN
            VALUE "FileVersion", "1.0.0.0\0"
            VALUE "ProductVersion", "1.0.0.0\0"
            VALUE "ProductName", "清风输入法\0"
            VALUE "FileDescription", "Windows TSF Chinese Input Method\0"
            VALUE "LegalCopyright", "Copyright (c) 2024\0"
        END
    END
    BLOCK "VarFileInfo"
    BEGIN
        VALUE "Translation", 0x0804, 0x04B0
    END
END
```

Then link into DLL and verify with:
```bash
wmic datafile where name="<path>\\wind_tsf.dll" get Version
```

<!-- MANUAL: Any manually added notes below this line are preserved on regeneration -->
