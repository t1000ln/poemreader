# Generates a tiny two-page, text-only PDF used to try out PDF support.
param(
    [string]$OutPath = (Join-Path $PSScriptRoot "..\samples\sample.pdf")
)

$ErrorActionPreference = "Stop"

function New-StreamObject {
    param([string]$Text)
    $Text = $Text -replace "`r`n", "`n"
    if (-not $Text.EndsWith("`n")) { $Text += "`n" }
    $length = [System.Text.Encoding]::ASCII.GetByteCount($Text)
    return "<< /Length $length >>`nstream`n$Text" + "endstream"
}

$page1 = @"
BT
/F1 24 Tf
72 760 Td
(PoemReader PDF Sample) Tj
ET
BT
/F1 13 Tf
72 712 Td
(This file checks PDF text extraction in PoemReader.) Tj
0 -26 Td
(Open it from the File menu; font size, colours and the) Tj
0 -26 Td
(reading position are remembered between runs.) Tj
0 -26 Td
(Only text-based PDFs are supported. Scanned pages) Tj
0 -26 Td
(contain no text layer and need OCR first.) Tj
ET
"@

$page2 = @"
BT
/F1 24 Tf
72 760 Td
(Second page) Tj
ET
BT
/F1 13 Tf
72 712 Td
(Page markers like this one separate PDF pages in the reader.) Tj
0 -26 Td
(The quick brown fox jumps over the lazy dog.) Tj
ET
"@

$objects = [System.Collections.Generic.List[string]]::new()
$objects.Add("<< /Type /Catalog /Pages 2 0 R >>")
$objects.Add("<< /Type /Pages /Kids [3 0 R 4 0 R] /Count 2 >>")
$objects.Add("<< /Type /Page /Parent 2 0 R /MediaBox [0 0 595 842] /Resources << /Font << /F1 7 0 R >> >> /Contents 5 0 R >>")
$objects.Add("<< /Type /Page /Parent 2 0 R /MediaBox [0 0 595 842] /Resources << /Font << /F1 7 0 R >> >> /Contents 6 0 R >>")
$objects.Add((New-StreamObject $page1))
$objects.Add((New-StreamObject $page2))
$objects.Add("<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>")

$builder = [System.Text.StringBuilder]::new()
[void]$builder.Append("%PDF-1.4`n")

$offsets = @()
for ($i = 0; $i -lt $objects.Count; $i++) {
    $offsets += $builder.Length
    [void]$builder.Append("$($i + 1) 0 obj`n$($objects[$i])`nendobj`n")
}

$xrefOffset = $builder.Length
[void]$builder.Append("xref`n0 $($objects.Count + 1)`n")
[void]$builder.Append("0000000000 65535 f `n")
foreach ($offset in $offsets) {
    [void]$builder.Append(("{0:D10} 00000 n `n" -f $offset))
}
[void]$builder.Append("trailer`n<< /Size $($objects.Count + 1) /Root 1 0 R >>`nstartxref`n$xrefOffset`n%%EOF`n")

[System.IO.File]::WriteAllText($OutPath, $builder.ToString(), [System.Text.Encoding]::ASCII)
Write-Host "Wrote $OutPath"
