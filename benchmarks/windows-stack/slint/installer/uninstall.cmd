@echo off
setlocal

rem Remove per-user registrations before deleting the installed files.
set "installDir=%LOCALAPPDATA%\Programs\QuickNoteSlintSpike"
if exist "%installDir%\QuickNoteSlintSpike.exe" "%installDir%\QuickNoteSlintSpike.exe" --unregister
reg delete "HKCU\Software\Microsoft\Windows\CurrentVersion\Uninstall\QuickNoteSlintSpike" /f >nul 2>&1

rem A detached process retries while this running script releases its file handle.
start "" /b powershell.exe -NoProfile -WindowStyle Hidden -Command "$target='%installDir%'; for($attempt=0; $attempt -lt 60; $attempt++){ try { Remove-Item -LiteralPath $target -Recurse -Force -ErrorAction Stop; break } catch { Start-Sleep -Milliseconds 500 } }"
exit /b 0
