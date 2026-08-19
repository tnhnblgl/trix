# Regenerates crates/trix-ui/icon-source.png: the Trix mark, cut out of the
# brand artwork and centred in a square, ready to feed to `cargo tauri icon`.
#
# Why a script rather than a hand-cropped PNG: assets/logo-transparent.png is
# the artwork as exported, and the mark sits off-centre in a mostly empty 1024
# frame -- 270px of margin on the left against 203 on the right, and it fills
# barely half the width. Dropped into an icon box unmodified it renders small
# and visibly left of centre. The crop is measured off the alpha channel on
# every run, so re-exporting the artwork at a different size or position
# cannot silently skew the icon.
#
# NOTE: this is the APP icon only. The tray icon is drawn in code, in
# trix-daemon's tray.rs, and is deliberately unrelated to this file: a white
# alpha ramp with two states (armed and idle) so it stays legible on light and
# dark taskbars and so the state is readable at 16px. The two are allowed to
# differ. Do not "fix" that by wiring them together -- a colour mark cannot
# show armed vs idle, which is the tray icon's whole job.
#
# After running this:  cd crates/trix-ui; cargo tauri icon icon-source.png
Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
Add-Type -AssemblyName System.Drawing

$src  = Join-Path $PSScriptRoot '..\assets\logo-transparent.png'
$dest = Join-Path $PSScriptRoot '..\crates\trix-ui\icon-source.png'

$side = 1024

# Fraction of the box the mark's longer axis fills. The mark is a right-pointing
# arrow carrying its own internal whitespace, so filling the box edge to edge
# reads as oversized beside other taskbar icons; 0.92 is where a logo mark
# normally sits inside a Windows icon.
$fill = 0.92

# Alpha at or below this counts as background when measuring the crop. The
# cutout's edges are hard rather than feathered, so any threshold works here;
# 8 just discards stray near-transparent pixels rather than letting one of
# them define the bounding box.
$floor = 8

$logo = New-Object System.Drawing.Bitmap $src

# LockBits rather than GetPixel: this is a million pixels, and GetPixel makes
# it a million interop calls.
$all = New-Object System.Drawing.Rectangle 0, 0, $logo.Width, $logo.Height
$bits = $logo.LockBits($all, [System.Drawing.Imaging.ImageLockMode]::ReadOnly,
                       [System.Drawing.Imaging.PixelFormat]::Format32bppArgb)
$bytes = New-Object 'byte[]' ($bits.Stride * $logo.Height)
[System.Runtime.InteropServices.Marshal]::Copy($bits.Scan0, $bytes, 0, $bytes.Length)
$stride = $bits.Stride
$logo.UnlockBits($bits)

$minX = $logo.Width; $minY = $logo.Height; $maxX = -1; $maxY = -1
for ($y = 0; $y -lt $logo.Height; $y++) {
  $row = $y * $stride
  for ($x = 0; $x -lt $logo.Width; $x++) {
    # BGRA in memory, so alpha is the fourth byte of the pixel.
    if ($bytes[$row + $x * 4 + 3] -le $floor) { continue }
    if ($x -lt $minX) { $minX = $x }
    if ($x -gt $maxX) { $maxX = $x }
    if ($y -lt $minY) { $minY = $y }
    if ($y -gt $maxY) { $maxY = $y }
  }
}
if ($maxX -lt 0) { throw "$src has no opaque pixels -- is it the right file?" }

$cropW = $maxX - $minX + 1
$cropH = $maxY - $minY + 1
$scale = ($side * $fill) / [math]::Max($cropW, $cropH)
$drawW = [int][math]::Round($cropW * $scale)
$drawH = [int][math]::Round($cropH * $scale)
$drawX = [int][math]::Round(($side - $drawW) / 2)
$drawY = [int][math]::Round(($side - $drawH) / 2)

$icon = New-Object System.Drawing.Bitmap $side, $side
$g = [System.Drawing.Graphics]::FromImage($icon)
$g.Clear([System.Drawing.Color]::Transparent)
$g.InterpolationMode = [System.Drawing.Drawing2D.InterpolationMode]::HighQualityBicubic
$g.PixelOffsetMode = [System.Drawing.Drawing2D.PixelOffsetMode]::HighQuality
$g.CompositingQuality = [System.Drawing.Drawing2D.CompositingQuality]::HighQuality

# TileFlipXY, or GDI+ samples past the edge of the crop rectangle and rings the
# mark with a half-transparent halo.
$attr = New-Object System.Drawing.Imaging.ImageAttributes
$attr.SetWrapMode([System.Drawing.Drawing2D.WrapMode]::TileFlipXY)
$dst = New-Object System.Drawing.Rectangle $drawX, $drawY, $drawW, $drawH
$g.DrawImage($logo, $dst, $minX, $minY, $cropW, $cropH,
             [System.Drawing.GraphicsUnit]::Pixel, $attr)

$icon.Save($dest, [System.Drawing.Imaging.ImageFormat]::Png)
$attr.Dispose(); $g.Dispose(); $icon.Dispose(); $logo.Dispose()

Write-Host "measured mark at ($minX,$minY) ${cropW}x${cropH} in $(Split-Path $src -Leaf)"
Write-Host "wrote $dest -- ${side}x${side}, mark ${drawW}x${drawH} at ($drawX,$drawY)"
