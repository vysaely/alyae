param([string]$OutDir)
# Draws Alyae's icon: a violet rounded square with two account figures; writes alyae.png (256)
# and alyae.ico (16-256, PNG entries) into $OutDir.
Add-Type -AssemblyName System.Drawing
$ErrorActionPreference = 'Stop'
New-Item -ItemType Directory -Force $OutDir | Out-Null

function RoundRect([float]$x, [float]$y, [float]$w, [float]$h, [float]$r) {
    $p = New-Object System.Drawing.Drawing2D.GraphicsPath
    $d = 2 * $r
    $p.AddArc($x, $y, $d, $d, 180, 90)
    $p.AddArc($x + $w - $d, $y, $d, $d, 270, 90)
    $p.AddArc($x + $w - $d, $y + $h - $d, $d, $d, 0, 90)
    $p.AddArc($x, $y + $h - $d, $d, $d, 90, 90)
    $p.CloseFigure()
    $p
}

# a figure: a round head and shoulders (the top of an ellipse, cut flat at $cut)
function Figure([float]$cx, [float]$headY, [float]$headR, [float]$bodyW, [float]$bodyTop, [float]$cut) {
    $p = New-Object System.Drawing.Drawing2D.GraphicsPath
    $p.AddEllipse($cx - $headR, $headY - $headR, 2 * $headR, 2 * $headR)
    $body = New-Object System.Drawing.Drawing2D.GraphicsPath
    $body.AddEllipse($cx - $bodyW / 2, $bodyTop, $bodyW, ($cut - $bodyTop) * 2)
    $clip = New-Object System.Drawing.Region($body)
    $clip.Intersect((New-Object System.Drawing.RectangleF(0, 0, 1024, $cut)))
    @{ head = $p; body = $clip }
}

$S = 1024
$bmp = New-Object System.Drawing.Bitmap($S, $S, [System.Drawing.Imaging.PixelFormat]::Format32bppArgb)
$g = [System.Drawing.Graphics]::FromImage($bmp)
$g.SmoothingMode = 'AntiAlias'
$g.PixelOffsetMode = 'HighQuality'
$g.Clear([System.Drawing.Color]::Transparent)

$tile = RoundRect 40 40 944 944 230
$grad = New-Object System.Drawing.Drawing2D.LinearGradientBrush(
    (New-Object System.Drawing.PointF(40, 40)), (New-Object System.Drawing.PointF(984, 984)),
    [System.Drawing.Color]::FromArgb(255, 0xB0, 0xA2, 0xFF), [System.Drawing.Color]::FromArgb(255, 0x56, 0x41, 0xD8))
$g.FillPath($grad, $tile)
# a soft light along the top
$shine = New-Object System.Drawing.Drawing2D.LinearGradientBrush(
    (New-Object System.Drawing.PointF(0, 40)), (New-Object System.Drawing.PointF(0, 520)),
    [System.Drawing.Color]::FromArgb(60, 255, 255, 255), [System.Drawing.Color]::FromArgb(0, 255, 255, 255))
$g.SetClip($tile)
# only over the top: past its end the gradient would start again as a band
$g.SetClip((New-Object System.Drawing.RectangleF(0, 0, 1024, 519)), 'Intersect')
$g.FillPath($shine, $tile)
$g.SetClip($tile)

$back = Figure 640 350 118 400 520 800
$front = Figure 430 440 140 470 650 860
$soft = New-Object System.Drawing.SolidBrush([System.Drawing.Color]::FromArgb(150, 255, 255, 255))
$white = New-Object System.Drawing.SolidBrush([System.Drawing.Color]::White)
$g.FillPath($soft, $back.head)
$g.FillRegion($soft, $back.body)
# the front figure is cut out of the back one by a violet ring
$ring = New-Object System.Drawing.Pen([System.Drawing.Color]::FromArgb(255, 0x7A, 0x67, 0xEE), 44)
$g.DrawPath($ring, $front.head)
$frontBody = New-Object System.Drawing.Drawing2D.GraphicsPath
$frontBody.AddEllipse(430 - 235, 650, 470, (860 - 650) * 2)
# the ring follows the shoulders only, not the part of the ellipse below the cut
$g.SetClip((New-Object System.Drawing.RectangleF(0, 0, 1024, 860)), 'Intersect')
$g.DrawPath($ring, $frontBody)
$g.SetClip($tile)
$g.FillPath($white, $front.head)
$g.FillRegion($white, $front.body)
$g.Dispose()

function Scaled([int]$n) {
    $b = New-Object System.Drawing.Bitmap($n, $n, [System.Drawing.Imaging.PixelFormat]::Format32bppArgb)
    $gg = [System.Drawing.Graphics]::FromImage($b)
    $gg.InterpolationMode = 'HighQualityBicubic'
    $gg.PixelOffsetMode = 'HighQuality'
    $gg.SmoothingMode = 'AntiAlias'
    $gg.DrawImage($bmp, 0, 0, $n, $n)
    $gg.Dispose()
    $ms = New-Object System.IO.MemoryStream
    $b.Save($ms, [System.Drawing.Imaging.ImageFormat]::Png)
    $b.Dispose()
    , $ms.ToArray()
}

$sizes = 16, 24, 32, 48, 64, 128, 256
$pngs = @{}
foreach ($n in $sizes) { $pngs[$n] = Scaled $n }
[IO.File]::WriteAllBytes((Join-Path $OutDir 'alyae.png'), $pngs[256])

# ICO: header, one 16-byte entry per size, then the PNGs
$ms = New-Object System.IO.MemoryStream
$w = New-Object System.IO.BinaryWriter($ms)
$w.Write([uint16]0); $w.Write([uint16]1); $w.Write([uint16]$sizes.Count)
$offset = 6 + 16 * $sizes.Count
foreach ($n in $sizes) {
    $dim = if ($n -ge 256) { 0 } else { $n }
    $w.Write([byte]$dim); $w.Write([byte]$dim); $w.Write([byte]0); $w.Write([byte]0)
    $w.Write([uint16]1); $w.Write([uint16]32)
    $w.Write([uint32]$pngs[$n].Length); $w.Write([uint32]$offset)
    $offset += $pngs[$n].Length
}
foreach ($n in $sizes) { $w.Write($pngs[$n]) }
$w.Flush()
[IO.File]::WriteAllBytes((Join-Path $OutDir 'alyae.ico'), $ms.ToArray())
$bmp.Save((Join-Path $env:TEMP 'alyae-icon-preview.png'), [System.Drawing.Imaging.ImageFormat]::Png)
"wrote alyae.png ($($pngs[256].Length) bytes) and alyae.ico ($($ms.Length) bytes)"
