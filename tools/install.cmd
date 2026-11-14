@echo off
rem Installs the PoemReader in this folder for the current user: copies it to
rem %LOCALAPPDATA%\Programs\PoemReader, adds a Start Menu shortcut, an entry in
rem Settings > Apps, and registers it as a candidate for its file types.
rem
rem Double-click to install; run "install.cmd -Uninstall" to remove it.
powershell -NoProfile -ExecutionPolicy Bypass -File "%~dp0install.ps1" %*

rem Keep the window open when started by double-click, so the result stays visible.
if "%~1"=="" pause
