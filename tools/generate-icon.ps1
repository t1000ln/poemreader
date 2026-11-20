# Regenerates assets/icon.ico (multi-size, PNG entries) and assets/icon-256.png.
#
# The mark is a sheet of paper — text lines above, a photo below — rising out of
# a printer: the program reads text documents and images, and prints them. Small
# sizes (16/24/32) are drawn on their own with heavier strokes instead of being
# downscaled, because a 256 px drawing loses every hairline on the taskbar.
param(
    [string]$OutDir = (Join-Path $PSScriptRoot "..\assets")
)

Add-Type -AssemblyName System.Drawing

$Canvas = 256

function New-RoundedRectPath {
    param([float]$X, [float]$Y, [float]$W, [float]$H, [float]$R)
    $path = New-Object System.Drawing.Drawing2D.GraphicsPath
    $r = [Math]::Min($R, [Math]::Min($W, $H) / 2.0)
    if ($r -le 0.01) {
        $path.AddRectangle((New-Object System.Drawing.RectangleF($X, $Y, $W, $H)))
        return $path
    }
    $d = $r * 2
    $path.AddArc($X, $Y, $d, $d, 180, 90)
    $path.AddArc($X + $W - $d, $Y, $d, $d, 270, 90)
    $path.AddArc($X + $W - $d, $Y + $H - $d, $d, $d, 0, 90)
    $path.AddArc($X, $Y + $H - $d, $d, $d, 90, 90)
    $path.CloseFigure()
    return $path
}

# A page: rounded at the top, square at the bottom, because the bottom edge is
# tucked into the printer and must cut clean against the front cover.
function New-PagePath {
    param([float]$X, [float]$Y, [float]$W, [float]$H, [float]$R)
    $path = New-Object System.Drawing.Drawing2D.GraphicsPath
    $d = $R * 2
    $path.AddArc($X, $Y, $d, $d, 180, 90)
    $path.AddArc($X + $W - $d, $Y, $d, $d, 270, 90)
    $path.AddLine($X + $W, $Y + $H, $X, $Y + $H)
    $path.CloseFigure()
    return $path
}

function New-VerticalBrush {
    param([float]$Top, [float]$Bottom, [string]$From, [string]$To)
    $rect = New-Object System.Drawing.RectangleF(0, $Top, 256, [Math]::Max($Bottom - $Top, 1))
    $brush = New-Object System.Drawing.Drawing2D.LinearGradientBrush(
        $rect,
        [System.Drawing.ColorTranslator]::FromHtml($From),
        [System.Drawing.ColorTranslator]::FromHtml($To),
        [System.Drawing.Drawing2D.LinearGradientMode]::Vertical
    )
    return $brush
}

function New-DiagonalBrush {
    param([float]$X, [float]$Y, [float]$W, [float]$H, [string]$From, [string]$To)
    $rect = New-Object System.Drawing.RectangleF($X, $Y, $W, $H)
    $brush = New-Object System.Drawing.Drawing2D.LinearGradientBrush(
        $rect,
        [System.Drawing.ColorTranslator]::FromHtml($From),
        [System.Drawing.ColorTranslator]::FromHtml($To),
        115.0
    )
    return $brush
}

function New-AppIcon {
    param([int]$Size)

    # Below 48 px the drawing is simplified: fewer details, heavier strokes.
    # Below 32 px only one text line survives, so the sheet still says
    # "document" instead of turning into grey noise.
    $small = $Size -lt 48
    $tiny = $Size -lt 32

    $bmp = New-Object System.Drawing.Bitmap($Size, $Size, [System.Drawing.Imaging.PixelFormat]::Format32bppArgb)
    $g = [System.Drawing.Graphics]::FromImage($bmp)
    $g.SmoothingMode = [System.Drawing.Drawing2D.SmoothingMode]::AntiAlias
    $g.PixelOffsetMode = [System.Drawing.Drawing2D.PixelOffsetMode]::HighQuality
    $g.CompositingQuality = [System.Drawing.Drawing2D.CompositingQuality]::HighQuality
    $g.Clear([System.Drawing.Color]::Transparent)
    # Everything below is laid out on a 256 px grid.
    $g.ScaleTransform(($Size / 256.0), ($Size / 256.0))

    # ---- badge -----------------------------------------------------------
    $badge = New-RoundedRectPath 8 8 240 240 54
    $g.FillPath((New-DiagonalBrush 8 8 240 240 "#5B8AE6" "#203C78"), $badge)
    if (-not $small) {
        $hl = New-RoundedRectPath 8 8 240 130 54
        $hlBrush = New-Object System.Drawing.Drawing2D.LinearGradientBrush(
            (New-Object System.Drawing.RectangleF(0, 8, 256, 132)),
            [System.Drawing.Color]::FromArgb(52, 255, 255, 255),
            [System.Drawing.Color]::FromArgb(0, 255, 255, 255),
            90.0
        )
        $g.FillPath($hlBrush, $hl)
        $hlBrush.Dispose()
        $hl.Dispose()
    }

    # ---- machine, back half ---------------------------------------------
    # The page is drawn between the two halves of the machine, so it reads as
    # coming out of an output slot rather than resting on a tray.
    $bodyTop = 164.0
    $bodyBottom = 222.0
    if (-not $small) {
        $seat = New-Object System.Drawing.SolidBrush([System.Drawing.Color]::FromArgb(38, 6, 18, 44))
        $seatPath = New-RoundedRectPath 40 212 176 12 6
        $g.FillPath($seat, $seatPath)
        $seat.Dispose(); $seatPath.Dispose()
    }
    $body = New-RoundedRectPath 22 $bodyTop 212 ($bodyBottom - $bodyTop) 18
    $g.FillPath((New-VerticalBrush $bodyTop $bodyBottom "#E8F1FD" "#87A3CB"), $body)
    $body.Dispose()

    # ---- the sheet -------------------------------------------------------
    $paperX = 73.0
    $paperY = 16.0
    $paperW = 110.0
    $paperH = 151.0
    $paper = New-PagePath $paperX $paperY $paperW $paperH 11
    $g.FillPath((New-VerticalBrush $paperY ($paperY + $paperH) "#FFFDF7" "#F4ECDF"), $paper)

    # Text lines: the document half of the mark.
    $inkX = 87.0
    $inkW = 82.0
    $lineH = if ($tiny) { 15.0 } elseif ($small) { 12.0 } else { 8.0 }
    $lineBrush = New-Object System.Drawing.SolidBrush([System.Drawing.Color]::FromArgb(255, 104, 130, 172))
    $paleBrush = New-Object System.Drawing.SolidBrush([System.Drawing.Color]::FromArgb(255, 149, 169, 202))
    if ($tiny) {
        $rows = @(
            @{ Y = 38.0; W = $inkW }
        )
    } elseif ($small) {
        # Two lines are enough at 32 px, and they keep the sheet from turning
        # into a grey smear.
        $rows = @(
            @{ Y = 36.0; W = $inkW },
            @{ Y = 58.0; W = ($inkW * 0.66) }
        )
    } else {
        $rows = @(
            @{ Y = 34.0; W = $inkW },
            @{ Y = 52.0; W = ($inkW * 0.72) },
            @{ Y = 70.0; W = ($inkW * 0.88) }
        )
    }
    for ($i = 0; $i -lt $rows.Count; $i++) {
        $row = $rows[$i]
        $bar = New-RoundedRectPath $inkX $row.Y $row.W $lineH ($lineH / 2)
        if ($i -eq 0) { $g.FillPath($lineBrush, $bar) } else { $g.FillPath($paleBrush, $bar) }
        $bar.Dispose()
    }
    $lineBrush.Dispose(); $paleBrush.Dispose()

    # Photo thumbnail: the image half of the mark.
    $photoY = if ($tiny) { 70.0 } else { 86.0 }
    $photoH = if ($tiny) { 72.0 } else { 60.0 }
    $photo = New-RoundedRectPath $inkX $photoY $inkW $photoH 7
    $state = $g.Save()
    $g.SetClip($photo, [System.Drawing.Drawing2D.CombineMode]::Replace)
    $g.FillRectangle((New-VerticalBrush $photoY ($photoY + $photoH) "#FFD180" "#F2896E"), $inkX, $photoY, $inkW, $photoH)
    # Sun.
    $sun = New-Object System.Drawing.SolidBrush([System.Drawing.Color]::FromArgb(255, 255, 248, 228))
    $g.FillEllipse($sun, ($inkX + 52), ($photoY + 8), 17, 17)
    $sun.Dispose()
    # Hills, back then front. Left out at 16-24 px, where they only add noise.
    if ($Size -ge 48) {
    $back = New-Object System.Drawing.SolidBrush([System.Drawing.Color]::FromArgb(255, 92, 122, 172))
    $backPts = @(
        (New-Object System.Drawing.PointF(($inkX - 2), ($photoY + $photoH))),
        (New-Object System.Drawing.PointF(($inkX + 23), ($photoY + 14))),
        (New-Object System.Drawing.PointF(($inkX + 52), ($photoY + $photoH)))
    )
    $g.FillPolygon($back, $backPts)
    $back.Dispose()
    $front = New-Object System.Drawing.SolidBrush([System.Drawing.Color]::FromArgb(255, 45, 73, 122))
    $frontPts = @(
        (New-Object System.Drawing.PointF(($inkX + 12), ($photoY + $photoH))),
        (New-Object System.Drawing.PointF(($inkX + 53), ($photoY + 22))),
        (New-Object System.Drawing.PointF(($inkX + 86), ($photoY + $photoH)))
    )
    $g.FillPolygon($front, $frontPts)
    $front.Dispose()
    }
    $g.Restore($state)
    $photo.Dispose()

    if (-not $small) {
        $edge = New-Object System.Drawing.Pen([System.Drawing.Color]::FromArgb(44, 146, 126, 92), 1.5)
        $g.DrawPath($edge, $paper)

        # The machine's lip shades the page just above the slot.
        $shade = New-Object System.Drawing.RectangleF(0, 146, 256, 28)
        $shadeBrush = New-Object System.Drawing.Drawing2D.LinearGradientBrush(
            $shade,
            [System.Drawing.Color]::FromArgb(0, 32, 44, 72),
            [System.Drawing.Color]::FromArgb(62, 32, 44, 72),
            90.0
        )
        $g.SetClip($paper, [System.Drawing.Drawing2D.CombineMode]::Replace)
        $g.FillRectangle($shadeBrush, 0, 146, 256, 28)
        $g.ResetClip()
        $shadeBrush.Dispose()
    }
    $paper.Dispose()

    # ---- slot rim --------------------------------------------------------
    # A narrow darker bar along the machine's top edge: the page disappears
    # under it, so it reads as the mouth of the output slot, and being only a
    # few pixels tall it never looks like a shelf.
    $rim = New-RoundedRectPath 24 ($bodyTop - 1) 208 5 2.5
    $rimBrush = New-VerticalBrush ($bodyTop - 1) ($bodyTop + 4) "#96ADC9" "#B7C9E0"
    $g.FillPath($rimBrush, $rim)
    $rimBrush.Dispose(); $rim.Dispose()

    # Lid seam: a hairline just under the rim, which is what separates a
    # printer's lid from its front face.
    if (-not $small) {
        $lidSeam = New-Object System.Drawing.Pen([System.Drawing.Color]::FromArgb(38, 96, 118, 158), 1.6)
        $g.DrawLine($lidSeam, 30, 174.5, 226, 174.5)
        $lidSeam.Dispose()
    }

    # Output slot on the front face: a dark slit reads as a printer even at
    # 16 px, where every other machine detail has already disappeared.
    $slotH = if ($small) { 9.0 } else { 7.0 }
    $slot = New-RoundedRectPath 66 186 124 $slotH 3.5
    $slotBrush = New-VerticalBrush 186 195 "#6F86AA" "#8CA2C1"
    $g.FillPath($slotBrush, $slot)
    $slotBrush.Dispose(); $slot.Dispose()

    if (-not $small) {
        $led = New-Object System.Drawing.SolidBrush([System.Drawing.Color]::FromArgb(255, 96, 214, 199))
        $g.FillEllipse($led, 200, 184, 9, 9)
        $led.Dispose()
    }

    $g.Dispose()
    return $bmp
}

if (-not (Test-Path $OutDir)) {
    New-Item -ItemType Directory -Path $OutDir | Out-Null
}

$sizes = @(16, 24, 32, 48, 64, 128, 256)
$pngs = @()
foreach ($size in $sizes) {
    $bitmap = New-AppIcon -Size $size
    if ($size -eq $Canvas) {
        $bitmap.Save((Join-Path $OutDir "icon-256.png"), [System.Drawing.Imaging.ImageFormat]::Png)
    }
    $ms = New-Object System.IO.MemoryStream
    $bitmap.Save($ms, [System.Drawing.Imaging.ImageFormat]::Png)
    $pngs += , $ms.ToArray()
    $ms.Dispose()
    $bitmap.Dispose()
}

# Assemble an ICO whose entries are PNG images (supported since Windows Vista).
$icoPath = Join-Path $OutDir "icon.ico"
$stream = New-Object System.IO.FileStream($icoPath, [System.IO.FileMode]::Create)
$writer = New-Object System.IO.BinaryWriter($stream)

$writer.Write([UInt16]0)                      # reserved
$writer.Write([UInt16]1)                      # type: icon
$writer.Write([UInt16]$sizes.Count)

$offset = 6 + (16 * $sizes.Count)
for ($i = 0; $i -lt $sizes.Count; $i++) {
    $dimension = if ($sizes[$i] -ge 256) { 0 } else { $sizes[$i] }
    $writer.Write([byte]$dimension)
    $writer.Write([byte]$dimension)
    $writer.Write([byte]0)                    # palette size
    $writer.Write([byte]0)                    # reserved
    $writer.Write([UInt16]1)                  # color planes
    $writer.Write([UInt16]32)                 # bits per pixel
    $writer.Write([UInt32]$pngs[$i].Length)
    $writer.Write([UInt32]$offset)
    $offset += $pngs[$i].Length
}
foreach ($data in $pngs) {
    $writer.Write($data)
}

$writer.Flush()
$writer.Dispose()
$stream.Dispose()

Write-Host "Wrote $icoPath and icon-256.png"
