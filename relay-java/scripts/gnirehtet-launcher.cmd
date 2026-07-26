@echo off
setlocal EnableExtensions EnableDelayedExpansion

set "SCRIPT_DIR=%~dp0"
cd /d "%SCRIPT_DIR%" >nul 2>&1
set "ACTION=%~1"
if not defined ACTION set "ACTION=run"

if /I "%ACTION%"=="repair" goto :repair_only
if /I "%ACTION%"=="status" goto :status_only

goto :run

:run
call :check_deps
call :print_header
call :print_status

if not "!REQUIRED_OK!"=="1" (
    echo.
    echo Required dependencies are missing.
    call :dependency_menu
    if errorlevel 1 exit /b 1
    goto :run
)

if not "!DEVICE_OK!"=="1" (
    echo.
    echo Quest is not visible as an authorized adb device yet.
    echo Put on the headset and accept the USB debugging prompt, then retry.
    call :device_menu
    if errorlevel 1 exit /b 1
    goto :run
)

echo.
echo [START] Starting Gnirehtet. Keep this window open while you use Virtual Desktop.
echo [STOP ] Press Ctrl+C in this window to stop reverse tethering.
echo.
java -jar gnirehtet.jar run
set "RUN_EXIT=%ERRORLEVEL%"
echo.
if "%RUN_EXIT%"=="0" (
    echo [OK] Gnirehtet stopped.
) else (
    echo [ERROR] Gnirehtet exited with code %RUN_EXIT%.
    echo Use gnirehtet-repair.cmd if adb or platform-tools look broken.
)
pause
exit /b %RUN_EXIT%

:status_only
call :check_deps
call :print_header
call :print_status
pause
exit /b 0

:repair_only
call :repair
pause
exit /b %ERRORLEVEL%

:dependency_menu
echo.
echo [R] Repair adb/platform-tools
echo [S] Show status again
echo [Q] Quit
set "CHOICE="
set /p "CHOICE=Choose an action: "
if /I "!CHOICE!"=="R" call :repair
if /I "!CHOICE!"=="Q" exit /b 1
exit /b 0

:device_menu
echo.
echo [Enter] Retry device check
echo [R] Repair adb/platform-tools
echo [Q] Quit
set "CHOICE="
set /p "CHOICE=Choose an action: "
if /I "!CHOICE!"=="R" call :repair
if /I "!CHOICE!"=="Q" exit /b 1
exit /b 0

:repair
call :print_header
echo [REPAIR] Checking local repair options...
echo.

if exist "gnirehtet-get-adb.cmd" (
    echo [REPAIR] Installing or refreshing Android platform-tools in this folder.
    call "gnirehtet-get-adb.cmd"
) else (
    echo [WARN] gnirehtet-get-adb.cmd is missing. Download a fresh release zip.
)

echo.
where java >nul 2>&1
if errorlevel 1 (
    echo [WARN] Java was not found in PATH.
    echo        Install a Java runtime, then run gnirehtet-run.cmd again.
    echo        Download: https://adoptium.net/temurin/releases/
) else (
    echo [OK] Java is available.
)

if not exist "gnirehtet.jar" echo [WARN] gnirehtet.jar is missing. Re-download the release zip.
if not exist "gnirehtet.apk" echo [WARN] gnirehtet.apk is missing. Re-download the release zip.

echo.
echo [REPAIR] Done. Returning to the launcher.
exit /b 0

:check_deps
set "JAVA_OK=0"
set "ADB_OK=0"
set "JAR_OK=0"
set "APK_OK=0"
set "DEVICE_OK=0"
set "REQUIRED_OK=0"
set "ADB_CMD="
set "DEVICE_STATE="

where java >nul 2>&1
if not errorlevel 1 set "JAVA_OK=1"

if exist "gnirehtet.jar" set "JAR_OK=1"
if exist "gnirehtet.apk" set "APK_OK=1"

if exist "%SCRIPT_DIR%adb.exe" (
    set "ADB_CMD=%SCRIPT_DIR%adb.exe"
) else (
    for /f "delims=" %%A in ('where adb 2^>nul') do if not defined ADB_CMD set "ADB_CMD=%%A"
)

if defined ADB_CMD (
    set "ADB_OK=1"
    set "ADB=!ADB_CMD!"
    for /f "delims=" %%S in ('"!ADB_CMD!" get-state 2^>nul') do if not defined DEVICE_STATE set "DEVICE_STATE=%%S"
    if /I "!DEVICE_STATE!"=="device" set "DEVICE_OK=1"
)

if "!JAVA_OK!!ADB_OK!!JAR_OK!!APK_OK!"=="1111" set "REQUIRED_OK=1"
exit /b 0

:print_header
cls
echo Gnirehtet Quest 3 Launcher
echo ===========================
echo.
exit /b 0

:print_status
echo Dependency status:
if "!JAVA_OK!"=="1" (echo [OK] Java runtime) else (echo [MISS] Java runtime)
if "!ADB_OK!"=="1" (echo [OK] adb: !ADB_CMD!) else (echo [MISS] adb / Android platform-tools)
if "!JAR_OK!"=="1" (echo [OK] gnirehtet.jar) else (echo [MISS] gnirehtet.jar)
if "!APK_OK!"=="1" (echo [OK] gnirehtet.apk) else (echo [MISS] gnirehtet.apk)
echo.
echo Quest status:
if "!ADB_OK!"=="1" (
    if "!DEVICE_OK!"=="1" (
        echo [OK] Authorized adb device detected.
    ) else (
        echo [WAIT] No authorized Quest detected.
        echo.
        "!ADB_CMD!" devices
    )
) else (
    echo [WAIT] adb is unavailable, so the Quest cannot be checked yet.
)
exit /b 0
