@echo off
setlocal

set "SCRIPT_DIR=%~dp0"
set "PLATFORM_TOOLS_VERSION=37.0.0"
set "ZIP_URL=https://dl.google.com/android/repository/platform-tools_r37.0.0-win.zip"
set "EXPECTED_SHA256=4fe305812db074cea32903a489d061eb4454cbc90a49e8fea677f4b7af764918"
set "ZIP_PATH=%TEMP%\platform-tools_r37.0.0-win.zip"
set "EXTRACT_DIR=%TEMP%\gnirehtet-platform-tools-%RANDOM%%RANDOM%"
set "FORCE_REFRESH=0"
if /I "%~1"=="--force" set "FORCE_REFRESH=1"

if "%FORCE_REFRESH%"=="0" if exist "%SCRIPT_DIR%adb.exe" if exist "%SCRIPT_DIR%AdbWinApi.dll" if exist "%SCRIPT_DIR%AdbWinUsbApi.dll" (
    echo Android Platform-Tools already exist next to Gnirehtet.
    echo You can now run gnirehtet-run.cmd.
    exit /b 0
)

echo Downloading Android Platform-Tools from Google...
powershell -NoProfile -ExecutionPolicy Bypass -Command ^
  "$ErrorActionPreference='Stop'; $ProgressPreference='SilentlyContinue'; Invoke-WebRequest -Uri $env:ZIP_URL -OutFile $env:ZIP_PATH"
if errorlevel 1 goto :fail

echo Verifying Android Platform-Tools %PLATFORM_TOOLS_VERSION% SHA-256...
powershell -NoProfile -ExecutionPolicy Bypass -Command ^
  "$ErrorActionPreference='Stop'; $actual=(Get-FileHash -Algorithm SHA256 -LiteralPath $env:ZIP_PATH).Hash.ToLowerInvariant(); if($actual -ne $env:EXPECTED_SHA256){Write-Error ('SHA-256 mismatch. Expected '+$env:EXPECTED_SHA256+', got '+$actual); exit 1}"
if errorlevel 1 goto :hash_fail

echo Extracting Android Platform-Tools...
powershell -NoProfile -ExecutionPolicy Bypass -Command ^
  "$ErrorActionPreference='Stop'; $ProgressPreference='SilentlyContinue'; Expand-Archive -LiteralPath $env:ZIP_PATH -DestinationPath $env:EXTRACT_DIR -Force"
if errorlevel 1 goto :fail

if not exist "%EXTRACT_DIR%\platform-tools\adb.exe" goto :fail

if exist "%SCRIPT_DIR%adb.exe" "%SCRIPT_DIR%adb.exe" kill-server >nul 2>&1

copy /Y "%EXTRACT_DIR%\platform-tools\adb.exe" "%SCRIPT_DIR%adb.exe" >nul
copy /Y "%EXTRACT_DIR%\platform-tools\AdbWinApi.dll" "%SCRIPT_DIR%AdbWinApi.dll" >nul
copy /Y "%EXTRACT_DIR%\platform-tools\AdbWinUsbApi.dll" "%SCRIPT_DIR%AdbWinUsbApi.dll" >nul

if not exist "%SCRIPT_DIR%adb.exe" goto :fail
if not exist "%SCRIPT_DIR%AdbWinApi.dll" goto :fail
if not exist "%SCRIPT_DIR%AdbWinUsbApi.dll" goto :fail

del /Q "%ZIP_PATH%" >nul 2>&1
rmdir /S /Q "%EXTRACT_DIR%" >nul 2>&1

echo Android Platform-Tools are ready.
echo You can now run gnirehtet-run.cmd.
exit /b 0

:fail
del /Q "%ZIP_PATH%" >nul 2>&1
rmdir /S /Q "%EXTRACT_DIR%" >nul 2>&1
echo Failed to download or extract Android Platform-Tools.
echo Download them manually from:
echo %ZIP_URL%
exit /b 1

:hash_fail
del /Q "%ZIP_PATH%" >nul 2>&1
rmdir /S /Q "%EXTRACT_DIR%" >nul 2>&1
echo Refusing to extract Android Platform-Tools because SHA-256 verification failed.
echo Expected: %EXPECTED_SHA256%
exit /b 1
