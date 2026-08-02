# Generates crates/trix-ui/icon-source.png: the tray glyph at app-icon size.
#
# Procedural rather than a checked-in binary so the app icon and the tray icon
# cannot drift apart, and so nobody has to find a PNG editor to change it. The
# shape is the ARMED tray icon of tray.rs -- a filled disc -- on the dark plate
# the app's own background uses, because an all-white glyph is invisible on a
# light taskbar.
Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
Add-Type -AssemblyName System.Drawing

$side = 512
$bmp = New-Object System.Drawing.Bitmap($side, $side)
$g = [System.Drawing.Graphics]::FromImage($bmp)
$g.SmoothingMode = [System.Drawing.Drawing2D.SmoothingMode]::AntiAlias
$g.Clear([System.Drawing.Color]::Transparent)

$plate = New-Object System.Drawing.SolidBrush ([System.Drawing.Color]::FromArgb(255, 22, 24, 29))
$path = New-Object System.Drawing.Drawing2D.GraphicsPath
$r = 96
$path.AddArc(0, 0, $r, $r, 180, 90)
$path.AddArc($side - $r, 0, $r, $r, 270, 90)
$path.AddArc($side - $r, $side - $r, $r, $r, 0, 90)
$path.AddArc(0, $side - $r, $r, $r, 90, 90)
$path.CloseFigure()
$g.FillPath($plate, $path)

$disc = New-Object System.Drawing.SolidBrush ([System.Drawing.Color]::FromArgb(255, 255, 255, 255))
$inset = 140
$g.FillEllipse($disc, $inset, $inset, $side - 2 * $inset, $side - 2 * $inset)

$out = Join-Path $PSScriptRoot '..\crates\trix-ui\icon-source.png'
$bmp.Save($out, [System.Drawing.Imaging.ImageFormat]::Png)
$g.Dispose(); $bmp.Dispose()
Write-Host "wrote $out"
