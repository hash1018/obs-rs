Add-Type -AssemblyName System.Drawing

function Add-RoundRect {
    param($Path, [double]$X, [double]$Y, [double]$W, [double]$H, [double]$R)
    $d = $R * 2
    if ($d -lt 1) { $Path.AddRectangle((New-Object System.Drawing.RectangleF $X, $Y, $W, $H)); return }
    $Path.AddArc($X, $Y, $d, $d, 180, 90)
    $Path.AddArc($X + $W - $d, $Y, $d, $d, 270, 90)
    $Path.AddArc($X + $W - $d, $Y + $H - $d, $d, $d, 0, 90)
    $Path.AddArc($X, $Y + $H - $d, $d, $d, 90, 90)
    $Path.CloseFigure()
}

function New-Master {
    param([int]$S)
    $bmp = New-Object System.Drawing.Bitmap $S, $S
    $g = [System.Drawing.Graphics]::FromImage($bmp)
    $g.SmoothingMode = 'AntiAlias'
    $g.Clear([System.Drawing.Color]::Transparent)

    # Dark rounded plate.
    $pad = $S * 0.035
    $plate = New-Object System.Drawing.Drawing2D.GraphicsPath
    Add-RoundRect $plate $pad $pad ($S - 2 * $pad) ($S - 2 * $pad) ($S * 0.225)
    $rect = New-Object System.Drawing.RectangleF $pad, $pad, ($S - 2 * $pad), ($S - 2 * $pad)
    $fill = New-Object System.Drawing.Drawing2D.LinearGradientBrush(
        $rect,
        [System.Drawing.Color]::FromArgb(255, 48, 54, 64),
        [System.Drawing.Color]::FromArgb(255, 22, 25, 31),
        90.0)
    $g.FillPath($fill, $plate)

    # The canvas: a filled 16:9 panel, which is what stays legible when this
    # is sixteen pixels across.
    $cw = $S * 0.60
    $ch = $cw * 9.0 / 16.0
    $cx = ($S - $cw) / 2.0
    $cy = ($S - $ch) / 2.0 - $S * 0.02
    $canvas = New-Object System.Drawing.Drawing2D.GraphicsPath
    Add-RoundRect $canvas $cx $cy $cw $ch ($S * 0.05)
    $g.FillPath((New-Object System.Drawing.SolidBrush ([System.Drawing.Color]::FromArgb(255, 226, 232, 240))), $canvas)

    # The record dot, inside the canvas rather than across its edge.
    $dr = $ch * 0.46
    $dx = $cx + $cw - $dr - $ch * 0.16
    $dy = $cy + $ch - $dr - $ch * 0.16
    $g.FillEllipse((New-Object System.Drawing.SolidBrush ([System.Drawing.Color]::FromArgb(255, 224, 49, 49))),
        $dx, $dy, $dr, $dr)

    $g.Dispose()
    return $bmp
}

$sizes = 256, 128, 64, 48, 32, 24, 16
$pngs = @()
foreach ($s in $sizes) {
    $b = New-Master $s
    $ms = New-Object System.IO.MemoryStream
    $b.Save($ms, [System.Drawing.Imaging.ImageFormat]::Png)
    $pngs += , @{ size = $s; bytes = $ms.ToArray() }
    $ms.Dispose(); $b.Dispose()
}

$out = New-Object System.IO.MemoryStream
$w = New-Object System.IO.BinaryWriter $out
$w.Write([uint16]0); $w.Write([uint16]1); $w.Write([uint16]$pngs.Count)
$offset = 6 + 16 * $pngs.Count
foreach ($p in $pngs) {
    $dim = if ($p.size -ge 256) { 0 } else { $p.size }
    $w.Write([byte]$dim); $w.Write([byte]$dim)
    $w.Write([byte]0); $w.Write([byte]0)
    $w.Write([uint16]1); $w.Write([uint16]32)
    $w.Write([uint32]$p.bytes.Length); $w.Write([uint32]$offset)
    $offset += $p.bytes.Length
}
foreach ($p in $pngs) { $w.Write($p.bytes) }
$w.Flush()
[System.IO.File]::WriteAllBytes($args[0], $out.ToArray())
$w.Dispose(); $out.Dispose()

# Preview: the real per-size renders, on light and dark, at true scale.
$sheet = New-Object System.Drawing.Bitmap 560, 420
$sg = [System.Drawing.Graphics]::FromImage($sheet)
$sg.Clear([System.Drawing.Color]::FromArgb(255, 245, 245, 247))
$sg.FillRectangle((New-Object System.Drawing.SolidBrush ([System.Drawing.Color]::FromArgb(255, 32, 34, 38))), 0, 300, 560, 120)
$x = 20
foreach ($s in 256, 64, 48, 32, 24, 16) {
    $sg.DrawImage((New-Master $s), $x, 20, $s, $s)
    $sg.DrawImage((New-Master $s), $x, 330, $s, $s)
    $x += $s + 18
}
$sg.Dispose()
$sheet.Save($args[1], [System.Drawing.Imaging.ImageFormat]::Png)
# The window icon, which is a separate thing from the executable resource: an
# application that sets its own window icon overrides what the .exe carries on
# Windows, and on Linux there is no executable resource at all. Read at startup
# by `eframe::icon_data::from_png_bytes`.
if ($args.Count -ge 3) {
    (New-Master 256).Save($args[2], [System.Drawing.Imaging.ImageFormat]::Png)
}

"wrote $($args[0]) ($((Get-Item $args[0]).Length) bytes)"
