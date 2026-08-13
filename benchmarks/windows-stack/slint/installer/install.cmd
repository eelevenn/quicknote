@echo off
setlocal

rem Install the spike for the current user without elevation.
set "installDir=%LOCALAPPDATA%\Programs\QuickNoteSlintSpike"
if not exist "%installDir%" mkdir "%installDir%"
copy /y "%~dp0quicknote-stack-slint.exe" "%installDir%\QuickNoteSlintSpike.exe" >nul
if errorlevel 1 exit /b 1
copy /y "%~dp0uninstall.cmd" "%installDir%\uninstall.cmd" >nul
if errorlevel 1 exit /b 1

rem Let the installed executable register its protocol and notification identity.
"%installDir%\QuickNoteSlintSpike.exe" --register
if errorlevel 1 exit /b 1

rem Publish a conventional per-user uninstall entry for Apps & Features.
reg add "HKCU\Software\Microsoft\Windows\CurrentVersion\Uninstall\QuickNoteSlintSpike" /v DisplayName /t REG_SZ /d "QuickNote Slint Integration Spike" /f >nul
reg add "HKCU\Software\Microsoft\Windows\CurrentVersion\Uninstall\QuickNoteSlintSpike" /v InstallLocation /t REG_SZ /d "%installDir%" /f >nul
reg add "HKCU\Software\Microsoft\Windows\CurrentVersion\Uninstall\QuickNoteSlintSpike" /v DisplayVersion /t REG_SZ /d "0.1.0" /f >nul
reg add "HKCU\Software\Microsoft\Windows\CurrentVersion\Uninstall\QuickNoteSlintSpike" /v NoModify /t REG_DWORD /d 1 /f >nul
reg add "HKCU\Software\Microsoft\Windows\CurrentVersion\Uninstall\QuickNoteSlintSpike" /v NoRepair /t REG_DWORD /d 1 /f >nul
reg add "HKCU\Software\Microsoft\Windows\CurrentVersion\Uninstall\QuickNoteSlintSpike" /v UninstallString /t REG_SZ /d "%installDir%\uninstall.cmd" /f >nul

rem The installer exits deterministically; the verification harness starts the app separately.
exit /b 0
