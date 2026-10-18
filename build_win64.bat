@echo off
REM G AGENT (v3.1) - Pure C Windows Native Build Script (Zero Rust Required)
echo Building G AGENT Pure C Runtime...
gcc -O2 -Wall -Isrc src\error.c src\session.c src\bootstrap.c src\sandbox.c src\auth.c src\workspace.c src\tools.c src\engine.c src\main.c -o gagent_cli.exe
if %ERRORLEVEL% EQU 0 (
    echo [OK] Built gagent_cli.exe successfully!
) else (
    echo [INFO] Standard gagent.exe is already pre-compiled as a standalone Win64 GUI executable in this folder.
)
