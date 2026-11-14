# Generates assets/icon.ico (multi-size, PNG-compressed entries) and a preview PNG.
# The icon is an open book on a blue rounded-square badge, matching assets/icon.svg.
param(
    [string]$OutDir = (Join-Path $PSScriptRoot "..\assets")
)

Add-Type -AssemblyName System.Drawing

$Canvas = 256

function New-RoundedRectPath {
    param([float]$X, [float]$Y, [float]$W, [float]$H, [float]$R)
    $path = New-Object System.Drawing.Drawing2D.GraphicsPath
    $d = $R * 2
    $path.AddArc($X, $Y, $d, $d, 180, 90)
    $path.AddArc($X + $W - $d, $Y, $d, $d, 270, 90)
    $path.AddArc($X + $W - $d, $Y + $H - $d, $d, $d, 0, 90)
    $path.AddArc($X, $Y + $H - $d, $d, $d, 90, 90)
    $path.CloseFigure()
    return $path
}

function New-BookBitmap {
    $bmp = New-Object System.Drawing.Bitmap($Canvas, $Canvas, [System.Drawing.Imaging.PixelFormat]::Format32bppArgb)
    $g = [System.Drawing.Graphics]::FromImage($bmp)
    $g.SmoothingMode = [System.Drawing.Drawing2D.SmoothingMode]::AntiAlias
    $g.PixelOffsetMode = [System.Drawing.Drawing2D.PixelOffsetMode]::HighQuality
    $g.CompositingQuality = [System.Drawing.Drawing2D.CompositingQuality]::HighQuality
    $g.Clear([System.Drawing.Color]::Transparent)

    # Badge background.
    $bgRect = New-Object System.Drawing.RectangleF(8, 8, 240, 240)
    $bgPath = New-RoundedRectPath 8 8 240 240 54
    $bgBrush = New-Object System.Drawing.Drawing2D.LinearGradientBrush(
        $bgRect,
        [System.Drawing.Color]::FromArgb(255, 84, 132, 224),
        [System.Drawing.Color]::FromArgb(255, 33, 62, 126),
        115.0
    )
    $g.FillPath($bgBrush, $bgPath)

    # Soft highlight across the top.
    $hlRect = New-Object System.Drawing.RectangleF(8, 8, 240, 118)
    $hlPath = New-RoundedRectPath 8 8 240 118 54
    $hlBrush = New-Object System.Drawing.Drawing2D.LinearGradientBrush(
        $hlRect,
        [System.Drawing.Color]::FromArgb(52, 255, 255, 255),
        [System.Drawing.Color]::FromArgb(0, 255, 255, 255),
        90.0
    )
    $g.FillPath($hlBrush, $hlPath)

    # Soft shadow under the book.
    $shadow = New-Object System.Drawing.SolidBrush([System.Drawing.Color]::FromArgb(52, 10, 22, 46))
    $shadowPath = New-Object System.Drawing.Drawing2D.GraphicsPath
    $shadowPath.AddEllipse(40, 148, 176, 56)
    $g.FillPath($shadow, $shadowPath)

    # Pages.
    $paper = New-Object System.Drawing.SolidBrush([System.Drawing.Color]::FromArgb(255, 250, 246, 234))
    $paperEdge = New-Object System.Drawing.Pen([System.Drawing.Color]::FromArgb(255, 225, 213, 187), 2.0)

    $left = New-Object System.Drawing.Drawing2D.GraphicsPath
    $left.StartFigure()
    $left.AddBezier(128, 96, 101, 84, 74, 80, 52, 82)
    $left.AddLine(52, 82, 52, 168)
    $left.AddBezier(52, 168, 74, 166, 101, 172, 128, 186)
    $left.CloseFigure()
    $g.FillPath($paper, $left)
    $g.DrawPath($paperEdge, $left)

    $right = New-Object System.Drawing.Drawing2D.GraphicsPath
    $right.StartFigure()
    $right.AddBezier(128, 96, 155, 84, 182, 80, 204, 82)
    $right.AddLine(204, 82, 204, 168)
    $right.AddBezier(204, 168, 182, 166, 155, 172, 128, 186)
    $right.CloseFigure()
    $g.FillPath($paper, $right)
    $g.DrawPath($paperEdge, $right)

    # Text lines on the pages.
    $ink = New-Object System.Drawing.Pen([System.Drawing.Color]::FromArgb(255, 150, 169, 202), 5.0)
    $ink.StartCap = [System.Drawing.Drawing2D.LineCap]::Round
    $ink.EndCap = [System.Drawing.Drawing2D.LineCap]::Round
    $g.DrawLine($ink, 68, 118, 110, 113)
    $g.DrawLine($ink, 68, 138, 110, 133)
    $g.DrawLine($ink, 70, 158, 98, 154)
    $g.DrawLine($ink, 146, 113, 188, 118)
    $g.DrawLine($ink, 146, 133, 188, 138)
    $g.DrawLine($ink, 158, 154, 186, 158)

    # Spine.
    $spineBrush = New-Object System.Drawing.SolidBrush([System.Drawing.Color]::FromArgb(255, 213, 195, 160))
    $spine = New-Object System.Drawing.Drawing2D.GraphicsPath
    $spine.AddBezier(121, 92, 125, 90, 131, 90, 135, 92)
    $spine.AddLine(135, 92, 135, 190)
    $spine.AddBezier(135, 190, 131, 188, 125, 188, 121, 190)
    $spine.CloseFigure()
    $g.FillPath($spineBrush, $spine)

    $spineShade = New-Object System.Drawing.Pen([System.Drawing.Color]::FromArgb(80, 118, 98, 64), 1.5)
    $g.DrawLine($spineShade, 128, 95, 128, 187)

    # Bookmark ribbon.
    $ribbonBrush = New-Object System.Drawing.SolidBrush([System.Drawing.Color]::FromArgb(255, 213, 86, 80))
    $ribbon = New-Object System.Drawing.PointF[] 5
    $ribbon[0] = New-Object System.Drawing.PointF(119, 86)
    $ribbon[1] = New-Object System.Drawing.PointF(137, 86)
    $ribbon[2] = New-Object System.Drawing.PointF(137, 148)
    $ribbon[3] = New-Object System.Drawing.PointF(128, 138)
    $ribbon[4] = New-Object System.Drawing.PointF(119, 148)
    $g.FillPolygon($ribbonBrush, $ribbon)

    $g.Dispose()
    return $bmp
}

$master = New-BookBitmap
$master.Save((Join-Path $OutDir "icon-256.png"), [System.Drawing.Imaging.ImageFormat]::Png)

$sizes = @(16, 24, 32, 48, 64, 128, 256)
$pngs = @()
foreach ($size in $sizes) {
    if ($size -eq $Canvas) {
        $ms = New-Object System.IO.MemoryStream
        $master.Save($ms, [System.Drawing.Imaging.ImageFormat]::Png)
        $pngs += , $ms.ToArray()
        $ms.Dispose()
        continue
    }

    $small = New-Object System.Drawing.Bitmap($size, $size, [System.Drawing.Imaging.PixelFormat]::Format32bppArgb)
    $sg = [System.Drawing.Graphics]::FromImage($small)
    $sg.InterpolationMode = [System.Drawing.Drawing2D.InterpolationMode]::HighQualityBicubic
    $sg.PixelOffsetMode = [System.Drawing.Drawing2D.PixelOffsetMode]::HighQuality
    $sg.CompositingQuality = [System.Drawing.Drawing2D.CompositingQuality]::HighQuality
    $sg.Clear([System.Drawing.Color]::Transparent)
    $dest = New-Object System.Drawing.Rectangle(0, 0, $size, $size)
    $sg.DrawImage($master, $dest, 0, 0, $Canvas, $Canvas, [System.Drawing.GraphicsUnit]::Pixel)
    $sg.Dispose()

    $ms = New-Object System.IO.MemoryStream
    $small.Save($ms, [System.Drawing.Imaging.ImageFormat]::Png)
    $pngs += , $ms.ToArray()
    $ms.Dispose()
    $small.Dispose()
}
$master.Dispose()

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
