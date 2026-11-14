param(
  [string]$Run = "",
  [string]$File = "",
  [int]$Wait = 2500,
  [string]$Shot = "_shot.png"
)
$ErrorActionPreference = 'Stop'
Add-Type -AssemblyName System.Drawing
Add-Type -AssemblyName System.Windows.Forms
Add-Type @"
using System;
using System.Runtime.InteropServices;
public class Ui2 {
  [DllImport("user32.dll")] public static extern bool SetForegroundWindow(IntPtr h);
}
"@
Get-Process poemreader -ErrorAction SilentlyContinue | Stop-Process -Force
Start-Sleep -Milliseconds 400
if ($File) { $p = Start-Process -FilePath $Run -ArgumentList $File -PassThru }
else { $p = Start-Process -FilePath $Run -PassThru }
Start-Sleep -Milliseconds $Wait
$p.Refresh()
[void][Ui2]::SetForegroundWindow($p.MainWindowHandle)
Start-Sleep -Milliseconds 400
$b = [System.Windows.Forms.SystemInformation]::VirtualScreen
$bmp = New-Object System.Drawing.Bitmap($b.Width, $b.Height)
$g = [System.Drawing.Graphics]::FromImage($bmp)
$g.CopyFromScreen($b.Left, $b.Top, 0, 0, $bmp.Size)
$bmp.Save($Shot, [System.Drawing.Imaging.ImageFormat]::Png)
$g.Dispose()
$bmp.Dispose()
$p | Select-Object MainWindowTitle
