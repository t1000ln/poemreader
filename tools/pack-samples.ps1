param(
    [switch]$Only
)

$ErrorActionPreference = "Stop"
Add-Type -AssemblyName System.IO.Compression
Add-Type -AssemblyName System.IO.Compression.FileSystem

# The sample documents are kept as unpacked OOXML parts under tools/, which makes
# them easy to read and edit; this script packs each folder into the archive that
# Office itself would write (entries separated by "/", not "\").
$jobs = @(
    @{ Source = "tools\sample-word"; Target = "samples\sample.docx" },
    @{ Source = "tools\sample-excel"; Target = "samples\sample.xlsx" }
)

foreach ($job in $jobs) {
    $source = (Resolve-Path $job.Source).Path
    $target = Join-Path (Get-Location) $job.Target
    Remove-Item $target -ErrorAction SilentlyContinue

    $zip = [System.IO.Compression.ZipFile]::Open($target, [System.IO.Compression.ZipArchiveMode]::Create)
    try {
        foreach ($file in Get-ChildItem $source -Recurse -File) {
            $relative = $file.FullName.Substring($source.Length + 1) -replace '\\', '/'
            [System.IO.Compression.ZipFileExtensions]::CreateEntryFromFile(
                $zip, $file.FullName, $relative,
                [System.IO.Compression.CompressionLevel]::Optimal) | Out-Null
        }
    }
    finally {
        $zip.Dispose()
    }

    $size = (Get-Item $target).Length
    "packed $($job.Target) : $size bytes"
}
