@echo off
setlocal
set "SCRIPT_DIR=%~dp0"
if not defined ADB if exist "%SCRIPT_DIR%adb.exe" set "ADB=%SCRIPT_DIR%adb.exe"
java -jar "%SCRIPT_DIR%gnirehtet.jar" %*
exit /b %ERRORLEVEL%
