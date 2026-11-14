@echo off
rem Registers the PoemReader.exe in this folder as a handler for the file types it
rem can open. Per-user only, no administrator rights needed.
rem
rem Double-click to register; run "register-associations.cmd -Unregister" to undo.
powershell -NoProfile -ExecutionPolicy Bypass -File "%~dp0register-associations.ps1" %*

rem Keep the window open when started by double-click, so the result stays visible.
if "%~1"=="" pause
