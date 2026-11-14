<#
.SYNOPSIS
    Installs PoemReader for the current user, or removes it again.

.DESCRIPTION
    Copies the application into a per-user folder, adds a Start Menu shortcut, an
    entry in Settings > Apps > Installed apps, and registers PoemReader as a
    candidate handler for the file types it can open (see
    register-associations.ps1, which does the registry work and is reused here so
    the type list lives in one place).

    Nothing is written outside HKCU and the user's own folders, so no administrator
    rights are needed and other accounts are unaffected.

    The script installs the copy that sits next to it, which is how the release
    archive is laid out: unpack anywhere, run install.cmd, and the folder can be
    deleted afterwards.

.PARAMETER InstallDir
    Target folder. Defaults to %LOCALAPPDATA%\Programs\PoemReader.

.PARAMETER NoAssociations
    Skip the file type registration; the app will still show up in "Open with"
    only if register-associations.ps1 is run later.

.PARAMETER NoShortcuts
    Skip the Start Menu shortcut.

.PARAMETER Uninstall
    Remove the installed copy, its shortcut, its registry entries and its file
    type registrations.

.PARAMETER Quiet
    Less output. Used by the uninstall entry in Settings so the window closes by
    itself.

.PARAMETER Root
    Registry path under HKCU. Only useful for testing.

.PARAMETER StartMenuDir
    Where the shortcut goes. Only useful for testing.

.EXAMPLE
    powershell -NoProfile -ExecutionPolicy Bypass -File install.ps1

.EXAMPLE
    powershell -NoProfile -ExecutionPolicy Bypass -File install.ps1 -InstallDir "D:\Apps\PoemReader"

.EXAMPLE
    powershell -NoProfile -ExecutionPolicy Bypass -File install.ps1 -Uninstall
#>
[CmdletBinding()]
param(
    [string]$InstallDir,
    [switch]$NoAssociations,
    [switch]$NoShortcuts,
    [switch]$Uninstall,
    [switch]$Quiet,
    [string]$Root = "Software",
    [string]$StartMenuDir = (Join-Path $env:APPDATA "Microsoft\Windows\Start Menu\Programs")
)

$ErrorActionPreference = "Stop"
$AppName = "PoemReader"
$ExeName = "PoemReader.exe"

# Everything the installer ships: the exe is required, the rest is copied when
# present so a stripped-down folder still installs.
$Files = @("PoemReader.exe", "README.md", "LICENSE-MIT", "LICENSE-APACHE",
    "THIRD-PARTY-NOTICES.md", "register-associations.ps1", "register-associations.cmd",
    "install.ps1", "install.cmd")
$Folders = @("samples")

$CurrentUser = [Microsoft.Win32.Registry]::CurrentUser
$UninstallKey = "$Root\Microsoft\Windows\CurrentVersion\Uninstall\$AppName"
$InstallKey = "$Root\$AppName\Install"

function Write-Status {
    param([string]$Message)
    if (-not $Quiet) { Write-Host $Message }
}

function Get-InstallDir {
    # The registry is the source of truth once installed; fall back to the folder the
    # script is running from, which is where the uninstall entry points anyway.
    if ($InstallDir) { return $InstallDir }

    $key = $CurrentUser.OpenSubKey($InstallKey)
    if ($key) {
        $stored = $key.GetValue("Dir")
        $key.Dispose()
        if ($stored -and (Test-Path -LiteralPath $stored -PathType Container)) {
            return $stored
        }
    }

    return $PSScriptRoot
}

function Get-SourceDir {
    $source = $PSScriptRoot
    if (-not (Test-Path -LiteralPath (Join-Path $source $ExeName) -PathType Leaf)) {
        throw "本脚本旁边没有 $ExeName。请把整个发布包解压后再运行，或用 -InstallDir 指定目标目录。"
    }
    return $source
}

function Install-App {
    $source = Get-SourceDir
    $target = $InstallDir
    if (-not $target) {
        $target = Join-Path $env:LOCALAPPDATA "Programs\$AppName"
    }
    $target = [IO.Path]::GetFullPath($target)
    $targetExe = Join-Path $target $ExeName

    if ($target.TrimEnd("\") -ieq $source.TrimEnd("\")) {
        throw "安装目录和发布包目录相同（$target）。请解压到别处再安装，或换一个 -InstallDir。"
    }

    Write-Status "正在安装到 $target"
    New-Item -ItemType Directory -Path $target -Force | Out-Null

    foreach ($name in $Files) {
        $from = Join-Path $source $name
        if (Test-Path -LiteralPath $from -PathType Leaf) {
            Copy-Item -LiteralPath $from -Destination (Join-Path $target $name) -Force
        }
    }
    foreach ($name in $Folders) {
        $from = Join-Path $source $name
        if (Test-Path -LiteralPath $from -PathType Container) {
            Copy-Item -LiteralPath $from -Destination $target -Recurse -Force
        }
    }

    $version = "0.1.0"
    try {
        $info = (Get-Item -LiteralPath $targetExe).VersionInfo
        if ($info.FileVersion) { $version = $info.FileVersion.Trim() }
    }
    catch { }

    if (-not $NoShortcuts) {
        New-Item -ItemType Directory -Path $StartMenuDir -Force | Out-Null
        $link = Join-Path $StartMenuDir "$AppName.lnk"
        $shell = New-Object -ComObject WScript.Shell
        $shortcut = $shell.CreateShortcut($link)
        $shortcut.TargetPath = $targetExe
        $shortcut.WorkingDirectory = $target
        $shortcut.IconLocation = "$targetExe,0"
        $shortcut.Description = "$AppName - 极简阅读器"
        $shortcut.Save()
        Write-Status "已添加快捷方式：$link"
    }

    # Settings > Apps reads this key; the uninstall command points back at the
    # installed copy of this script.
    $uninstallScript = Join-Path $target "install.ps1"
    $key = $CurrentUser.CreateSubKey($UninstallKey)
    $key.SetValue("DisplayName", $AppName)
    $key.SetValue("DisplayVersion", $version)
    $key.SetValue("Publisher", $AppName)
    $key.SetValue("DisplayIcon", "`"$targetExe`",0")
    $key.SetValue("InstallLocation", $target)
    $key.SetValue("UninstallString", "powershell -NoProfile -ExecutionPolicy Bypass -File `"$uninstallScript`" -Uninstall")
    $key.SetValue("QuietUninstallString", "powershell -NoProfile -ExecutionPolicy Bypass -File `"$uninstallScript`" -Uninstall -Quiet")
    $key.SetValue("NoModify", 1, [Microsoft.Win32.RegistryValueKind]::DWord)
    $key.SetValue("NoRepair", 1, [Microsoft.Win32.RegistryValueKind]::DWord)
    $size = 0
    Get-ChildItem -LiteralPath $target -Recurse -File -ErrorAction SilentlyContinue |
        ForEach-Object { $size += $_.Length }
    $key.SetValue("EstimatedSize", [int]($size / 1KB), [Microsoft.Win32.RegistryValueKind]::DWord)
    $key.Dispose()

    $install = $CurrentUser.CreateSubKey($InstallKey)
    $install.SetValue("Dir", $target)
    $install.SetValue("Version", $version)
    $install.Dispose()

    $associate = Join-Path $target "register-associations.ps1"
    if ($NoAssociations) {
        Write-Status "跳过文件关联（-NoAssociations）。"
    }
    elseif (Test-Path -LiteralPath $associate -PathType Leaf) {
        # Delegate to the shipped script so the type list is defined in one place.
        # -Root is passed through so a test run keeps its registry writes contained.
        & $associate -ExePath $targetExe -Root $Root | Out-Null
        Write-Status "已注册为以下格式的打开方式候选：文本、PDF、Word、Excel、Markdown、图片"
    }
    else {
        Write-Warning "找不到 register-associations.ps1，跳过文件关联。"
    }

    Write-Status ""
    Write-Status "安装完成：$target"
    Write-Status "之后可以在「打开方式」里选择 $AppName；要设为默认，请勾选「始终使用此应用」。"
    Write-Status "卸载：设置 -> 应用 -> 已安装的应用 -> $AppName，或运行 install.cmd -Uninstall"
}

function Uninstall-App {
    $target = Get-InstallDir
    $target = [IO.Path]::GetFullPath($target)
    $targetExe = Join-Path $target $ExeName

    Write-Status "正在卸载 $target"

    # A naive Remove-Item -Recurse on a path read from the registry is how installers
    # wipe the wrong folder. Only proceed when this really looks like our install.
    if (-not (Test-Path -LiteralPath $targetExe -PathType Leaf)) {
        Write-Warning "$target 下没有 $ExeName，跳过删除文件。"
    }
    elseif ($target -match '^[A-Za-z]:\\?$' -or -not (Split-Path -Parent $target)) {
        Write-Warning "$target 看起来是驱动器根目录，拒绝删除。"
    }
    else {
        $associate = Join-Path $target "register-associations.ps1"
        if (Test-Path -LiteralPath $associate -PathType Leaf) {
            & $associate -Unregister -Root $Root | Out-Null
            Write-Status "已移除文件关联。"
        }
        else {
            Write-Warning "找不到 register-associations.ps1，文件关联可能残留。"
        }

        if (-not $NoShortcuts) {
            $link = Join-Path $StartMenuDir "$AppName.lnk"
            if (Test-Path -LiteralPath $link) {
                Remove-Item -LiteralPath $link -Force
                Write-Status "已删除快捷方式。"
            }
        }

        try {
            Remove-Item -LiteralPath $target -Recurse -Force
        }
        catch {
            Write-Warning "部分文件无法删除（可能仍在使用）：$($_.Exception.Message)"
            Write-Warning "关闭 PoemReader 后手动删除 $target 即可。"
        }
    }

    $CurrentUser.DeleteSubKeyTree($UninstallKey, $false)
    $CurrentUser.DeleteSubKeyTree($InstallKey, $false)

    Write-Status "卸载完成。"
}

if ($Uninstall) {
    Uninstall-App
}
else {
    Install-App
}
