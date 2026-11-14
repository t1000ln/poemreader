# Generates a PDF that exercises text, vector tables and an embedded JPEG, so the
# renderer can be checked against more than plain text.
param(
    [string]$OutPath = (Join-Path $PSScriptRoot "..\samples\sample-rich.pdf")
)

$ErrorActionPreference = "Stop"
Add-Type -AssemblyName System.Drawing

# --- an embedded JPEG image ------------------------------------------------
$bitmap = New-Object System.Drawing.Bitmap 320, 200
$graphics = [System.Drawing.Graphics]::FromImage($bitmap)
$graphics.SmoothingMode = [System.Drawing.Drawing2D.SmoothingMode]::AntiAlias
$gradient = New-Object System.Drawing.Drawing2D.LinearGradientBrush(
    (New-Object System.Drawing.Point 0, 0),
    (New-Object System.Drawing.Point 320, 200),
    [System.Drawing.Color]::FromArgb(255, 37, 99, 235),
    [System.Drawing.Color]::FromArgb(255, 219, 39, 119))
$graphics.FillRectangle($gradient, 0, 0, 320, 200)
$graphics.FillEllipse([System.Drawing.Brushes]::White, 110, 50, 100, 100)
$graphics.FillEllipse([System.Drawing.Brushes]::Black, 145, 85, 30, 30)
$label = New-Object System.Drawing.Font("Arial", 20, [System.Drawing.FontStyle]::Bold)
$graphics.DrawString("IMAGE", $label, [System.Drawing.Brushes]::White, 8, 8)
$graphics.Dispose()
$imageStream = New-Object System.IO.MemoryStream
$bitmap.Save($imageStream, [System.Drawing.Imaging.ImageFormat]::Jpeg)
$jpeg = $imageStream.ToArray()
$bitmap.Dispose()
$imageStream.Dispose()

# --- page content ----------------------------------------------------------
$rows = @(
    @("Rust", "2015", "high"),
    @("Go", "2009", "high"),
    @("Python", "1991", "very high")
)

$table = ""
$top = 520
$rowHeight = 26
$left = 72
$columnWidths = @(150, 150, 151)

# A shaded header row, then one text run per cell.
$table += "0.90 0.93 0.98 rg $left $($top - $rowHeight) 451 $rowHeight re f 0 0 0 rg`n"
$headers = @("Language", "Year", "Stars")
$x = $left
for ($column = 0; $column -lt $headers.Count; $column++) {
    $table += "BT /F1 12 Tf $($x + 8) $($top - $rowHeight + 10) Td ($($headers[$column])) Tj ET`n"
    $x += $columnWidths[$column]
}
for ($row = 0; $row -lt $rows.Count; $row++) {
    $y = $top - ($row + 2) * $rowHeight
    $x = $left
    for ($column = 0; $column -lt $rows[$row].Count; $column++) {
        $table += "BT /F1 12 Tf $($x + 8) $($y + 9) Td ($($rows[$row][$column])) Tj ET`n"
        $x += $columnWidths[$column]
    }
}

$table += "0.55 0.58 0.64 RG 0.7 w`n"
for ($row = 0; $row -le $rows.Count; $row++) {
    $y = $top - $row * $rowHeight
    $table += "$left $y m $($left + 451) $y l S`n"
}
for ($column = 0; $column -le 3; $column++) {
    $x = $left
    for ($i = 0; $i -lt $column; $i++) { $x += $columnWidths[$i] }
    $table += "$x $top m $x $($top - $rows.Count * $rowHeight) l S`n"
}

$page1 = @"
BT
/F1 24 Tf
72 780 Td
(PoemReader Render Sample) Tj
ET
BT
/F1 12 Tf
72 752 Td
(Fonts, vector tables and an embedded JPEG image on one page.) Tj
ET
q
240 0 0 150 72 580 cm
/Im1 Do
Q
BT
/F1 11 Tf
72 558 Td
(A JPEG drawn through the image XObject operator above.) Tj
ET
$table
"@

$page2 = @"
BT
/F1 24 Tf
72 780 Td
(Second page) Tj
ET
BT
/F1 12 Tf
72 752 Td
(Plain text, used to check page navigation and the reading position.) Tj
ET
"@

function New-StreamObject {
    param([string]$Text)
    $Text = $Text -replace "`r`n", "`n"
    if (-not $Text.EndsWith("`n")) { $Text += "`n" }
    $length = [System.Text.Encoding]::ASCII.GetByteCount($Text)
    return "<< /Length $length >>`nstream`n$Text" + "endstream"
}

# Object order: catalog, pages, page 1, page 2, contents 1, image, font, contents 2.
$objects = [System.Collections.Generic.List[object]]::new()
$objects.Add("<< /Type /Catalog /Pages 2 0 R >>")
$objects.Add("<< /Type /Pages /Kids [3 0 R 4 0 R] /Count 2 >>")
$objects.Add("<< /Type /Page /Parent 2 0 R /MediaBox [0 0 595 842] /Resources << /Font << /F1 7 0 R >> /XObject << /Im1 6 0 R >> >> /Contents 5 0 R >>")
$objects.Add("<< /Type /Page /Parent 2 0 R /MediaBox [0 0 595 842] /Resources << /Font << /F1 7 0 R >> >> /Contents 8 0 R >>")
$objects.Add((New-StreamObject $page1))
$objects.Add("IMAGE")
$objects.Add("<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>")
$objects.Add((New-StreamObject $page2))

$memory = New-Object System.IO.MemoryStream
$writer = New-Object System.IO.StreamWriter($memory, [System.Text.Encoding]::ASCII)
$writer.NewLine = "`n"
$writer.Write("%PDF-1.4`n")
$writer.Flush()

$offsets = @()
for ($i = 0; $i -lt $objects.Count; $i++) {
    $offsets += $memory.Position
    $writer.Write("$($i + 1) 0 obj`n")
    if ($objects[$i] -is [string] -and $objects[$i] -eq "IMAGE") {
        $writer.Write("<< /Type /XObject /Subtype /Image /Width 320 /Height 200 /ColorSpace /DeviceRGB /BitsPerComponent 8 /Filter /DCTDecode /Length $($jpeg.Length) >>`nstream`n")
        $writer.Flush()
        $memory.Write($jpeg, 0, $jpeg.Length)
        $writer.Write("`nendstream`nendobj`n")
    } else {
        $writer.Write("$($objects[$i])`nendobj`n")
    }
    $writer.Flush()
}

$xrefOffset = $memory.Position
$writer.Write("xref`n0 $($objects.Count + 1)`n")
$writer.Write("0000000000 65535 f `n")
foreach ($offset in $offsets) {
    $writer.Write(("{0:D10} 00000 n `n" -f $offset))
}
$writer.Write("trailer`n<< /Size $($objects.Count + 1) /Root 1 0 R >>`nstartxref`n$xrefOffset`n%%EOF`n")
$writer.Flush()

[System.IO.File]::WriteAllBytes($OutPath, $memory.ToArray())
$writer.Dispose()
$memory.Dispose()
Write-Host "Wrote $OutPath"
