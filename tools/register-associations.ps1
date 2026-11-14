<#
.SYNOPSIS
    Registers PoemReader as a handler for the file types it can open, or removes it.

.DESCRIPTION
    Everything is written under HKCU, so no administrator rights are needed and
    other accounts on the machine are unaffected.

    This script ships inside the release archive, next to PoemReader.exe. Run it
    from that folder and it registers the copy that sits beside it, whatever
    directory the archive was unpacked into; the executable is located relative to
    the script rather than the shell's working directory, so double-clicking the
    .cmd wrapper and running it from a terminal behave the same. A checkout of the
    repository has no exe next to tools\, so there it falls back to
    target\release and then dist\.

    For every supported extension the script adds an OpenWithProgids entry and a
    ProgID that points at the executable. The ProgID is what carries the command
    line and the icon; the OpenWithProgids entry is what makes the app appear in
    "Open with" and in Settings > Apps > Default apps. The Capabilities block is
    what lets Windows list the app under "Set defaults by app".

    The extension's default value and the UserChoice key are deliberately left
    alone. Since Windows 8 UserChoice is hash-protected: writing it makes Windows
    treat the association as tampered with, drop it, and ask the user again. The
    default handler therefore stays a one-time user decision in the "Open with"
    dialog.

    The behaviour of an existing default handler is also untouched: removing the
    app only deletes the values written by this script, never a whole key that may
    belong to another program.

.PARAMETER ExePath
    Executable to associate. Defaults to the PoemReader.exe beside this script
    (the packaged layout), then to target\release\poemreader.exe, then to a copy
    under dist\.

.PARAMETER Unregister
    Remove the ProgIDs, capability declarations and OpenWithProgids entries that
    this script creates, then notify the shell.

.PARAMETER Root
    Registry path under HKCU. Only useful for testing the script against a
    throwaway key.

.EXAMPLE
    powershell -NoProfile -ExecutionPolicy Bypass -File tools\register-associations.ps1

.EXAMPLE
    powershell -NoProfile -ExecutionPolicy Bypass -File tools\register-associations.ps1 -ExePath "C:\Apps\PoemReader\PoemReader.exe"

.EXAMPLE
    powershell -NoProfile -ExecutionPolicy Bypass -File tools\register-associations.ps1 -Unregister
#>
[CmdletBinding(SupportsShouldProcess = $true)]
param(
    [string]$ExePath,
    [switch]$Unregister,
    [string]$Root = "Software"
)

$ErrorActionPreference = "Stop"
$AppName = "PoemReader"

# One ProgID per document family, so Explorer shows a meaningful type name and
# every extension of a family shares the same command line.
# The names are display strings and are shown as-is by Windows.
$Types = @(
    @{ ProgId = "PoemReader.txt";  Name = "文本文档";  Ext = @(".txt") }
    @{ ProgId = "PoemReader.pdf";  Name = "PDF 文档";  Ext = @(".pdf") }
    @{ ProgId = "PoemReader.word"; Name = "Word 文档"; Ext = @(".docx", ".docm", ".dotx") }
    @{ ProgId = "PoemReader.doc";  Name = "Word 97-2003 文档"; Ext = @(".doc") }
    @{ ProgId = "PoemReader.excel"; Name = "Excel 工作簿"; Ext = @(".xlsx", ".xlsm", ".xltx") }
    @{ ProgId = "PoemReader.xls";  Name = "Excel 97-2003 工作簿"; Ext = @(".xls") }
    @{ ProgId = "PoemReader.markdown"; Name = "Markdown 文档"; Ext = @(".md", ".markdown", ".mdown", ".mkd", ".mkdn") }
    @{ ProgId = "PoemReader.image"; Name = "图片"; Ext = @(".jpg", ".jpeg", ".jpe", ".jfif", ".png", ".bmp", ".dib", ".webp", ".ico", ".gif", ".svg", ".svgz") }
)

# Command line for the ProgID. %1 is quoted so paths with spaces survive; only one
# argument is passed because the reader opens a single document per window.
$AppDescription = "极简阅读器：文本、PDF、Word、Excel、Markdown 与图片"

$CurrentUser = [Microsoft.Win32.Registry]::CurrentUser

function Resolve-TargetExe {
    param([string]$Path)

    if ($Path) {
        if (-not (Test-Path -LiteralPath $Path -PathType Leaf)) {
            throw "找不到可执行文件：$Path"
        }
        return (Resolve-Path -LiteralPath $Path).ProviderPath
    }

    # The packaged layout: the script sits in the folder the archive was unpacked
    # into, so the exe to register is the one beside it. Resolved from the script
    # path, not the working directory: a double-clicked .cmd and a terminal session
    # can start in different places.
    foreach ($name in @("PoemReader.exe", "poemreader.exe")) {
        $local = Join-Path $PSScriptRoot $name
        if (Test-Path -LiteralPath $local -PathType Leaf) {
            return (Resolve-Path -LiteralPath $local).ProviderPath
        }
    }

    # A repository checkout keeps the script in tools\; use the built binary.
    $release = Join-Path $PSScriptRoot "..\target\release\poemreader.exe"
    if (Test-Path -LiteralPath $release -PathType Leaf) {
        return (Resolve-Path -LiteralPath $release).ProviderPath
    }

    $packaged = Get-ChildItem -Path (Join-Path $PSScriptRoot "..\dist") `
        -Filter "PoemReader.exe" -Recurse -ErrorAction SilentlyContinue |
        Select-Object -First 1
    if ($packaged) {
        return $packaged.FullName
    }

    throw "找不到 PoemReader.exe：本脚本所在目录、..\target\release 和 ..\dist 都没有。请把本脚本放到 PoemReader.exe 旁边，或用 -ExePath 指定路径。"
}

function New-RegKey {
    param([string]$Path)
    return $CurrentUser.CreateSubKey($Path)
}

function Remove-RegKey {
    param([string]$Path)
    # $false = do not throw when the key is already gone.
    try {
        $CurrentUser.DeleteSubKeyTree($Path, $false)
    }
    catch {
        Write-Warning "删除 $Path 失败：$($_.Exception.Message)"
    }
}

function Register-Types {
    param([string]$Target)

    $classes = "$Root\Classes"
    $capabilities = "$Root\$AppName\Capabilities"
    $registeredApps = "$Root\RegisteredApplications"

    foreach ($type in $Types) {
        $progId = New-RegKey "$classes\$($type.ProgId)"
        $progId.SetValue("", $type.Name)
        # Windows uses FriendlyTypeName for the "Type" column when available.
        $progId.SetValue("FriendlyTypeName", $type.Name)

        $icon = $progId.CreateSubKey("DefaultIcon")
        $icon.SetValue("", "`"$Target`",0")
        $icon.Dispose()

        $command = $progId.CreateSubKey("shell\open\command")
        $command.SetValue("", "`"$Target`" `"%1`"")
        $command.Dispose()

        $progId.Dispose()

        foreach ($ext in $type.Ext) {
            $openWith = New-RegKey "$classes\$ext\OpenWithProgids"
            # REG_NONE with zero-length data is the documented form; the shell only
            # enumerates the value names.
            $openWith.SetValue($type.ProgId, (New-Object byte[] 0), [Microsoft.Win32.RegistryValueKind]::None)
            $openWith.Dispose()
        }
    }

    $cap = New-RegKey $capabilities
    $cap.SetValue("ApplicationName", $AppName)
    $cap.SetValue("ApplicationDescription", $AppDescription)
    $cap.SetValue("ApplicationIcon", "`"$Target`",0")

    $associations = $cap.CreateSubKey("FileAssociations")
    foreach ($type in $Types) {
        foreach ($ext in $type.Ext) {
            $associations.SetValue($ext, $type.ProgId)
        }
    }
    $associations.Dispose()
    $cap.Dispose()

    # The value is a path relative to HKCU, which is where the shell looks it up.
    $registered = New-RegKey $registeredApps
    $registered.SetValue($AppName, "$Root\$AppName\Capabilities")
    $registered.Dispose()
}

function Unregister-Types {
    $classes = "$Root\Classes"

    foreach ($type in $Types) {
        foreach ($ext in $type.Ext) {
            $openWithPath = "$classes\$ext\OpenWithProgids"
            $openWith = $CurrentUser.OpenSubKey($openWithPath, $true)
            if ($openWith) {
                if ($openWith.GetValueNames() -contains $type.ProgId) {
                    $openWith.DeleteValue($type.ProgId, $false)
                }
                $emptyOpenWith = ($openWith.ValueCount -eq 0 -and $openWith.SubKeyCount -eq 0)
                $openWith.Dispose()
                if ($emptyOpenWith) {
                    try { $CurrentUser.DeleteSubKey($openWithPath, $false) } catch { }
                }
            }

            # An extension key created by this script is left with no values and no
            # subkeys, so it can go. One owned by another program still holds its
            # default value or handler list, and must stay.
            $extKey = $CurrentUser.OpenSubKey("$classes\$ext", $true)
            if ($extKey) {
                $emptyExt = ($extKey.ValueCount -eq 0 -and $extKey.SubKeyCount -eq 0)
                $extKey.Dispose()
                if ($emptyExt) {
                    try { $CurrentUser.DeleteSubKey("$classes\$ext", $false) } catch { }
                }
            }
        }

        Remove-RegKey "$classes\$($type.ProgId)"
    }

    Remove-RegKey "$Root\$AppName"

    $registered = $CurrentUser.OpenSubKey("$Root\RegisteredApplications", $true)
    if ($registered) {
        if ($registered.GetValueNames() -contains $AppName) {
            $registered.DeleteValue($AppName, $false)
        }
        $registered.Dispose()
    }
}

function Update-Shell {
    if (-not ("PoemReaderShell" -as [type])) {
        Add-Type -Namespace PoemReaderShell -Name Api -MemberDefinition @'
[System.Runtime.InteropServices.DllImport("shell32.dll")]
public static extern void SHChangeNotify(int eventId, int flags, System.IntPtr item1, System.IntPtr item2);
'@
    }
    # SHCNE_ASSOCCHANGED / SHCNF_IDLIST: make Explorer re-read the associations now
    # instead of after the next sign-in.
    [PoemReaderShell.Api]::SHChangeNotify(0x08000000, 0, [IntPtr]::Zero, [IntPtr]::Zero)
}

# --------------------------------------------------------------------------- main

if ($Unregister) {
    if (-not $PSCmdlet.ShouldProcess("HKCU:\$Root", "移除 $AppName 的文件关联")) {
        return
    }

    Unregister-Types
    Update-Shell
    Write-Host "已移除 $AppName 的文件关联。"
    return
}

$target = Resolve-TargetExe $ExePath

Write-Host "可执行文件：$target"
Write-Host "将注册以下关联（当前用户，无需管理员）："
foreach ($type in $Types) {
    Write-Host ("  {0}: {1}" -f $type.Name, ($type.Ext -join " "))
}

if (-not $PSCmdlet.ShouldProcess("HKCU:\$Root\Classes", "注册 $($Types.Count) 组文件关联")) {
    return
}

Register-Types $target
Update-Shell

Write-Host ""
Write-Host "完成。现在可以在「打开方式」里选择 $AppName："
Write-Host "  右键文件 -> 打开方式 -> 选择其他应用 -> 勾选「始终使用此应用」"
Write-Host "也可以在「设置 -> 应用 -> 默认应用」中按文件类型或按应用指定。"
Write-Host "换到别的目录后需要重新运行本脚本，注册表里记的是绝对路径。"
