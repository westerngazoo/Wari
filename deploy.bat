@echo off
setlocal enabledelayedexpansion

REM ============================================================
REM   wari deploy.bat — build, verify, commit, push
REM
REM   Usage:
REM     deploy.bat                          builds vf2 (default), msg = "wari deploy"
REM     deploy.bat "msg"                    builds vf2, custom commit msg
REM     deploy.bat vf2  "msg"               builds vf2 explicitly
REM     deploy.bat qemu "msg"               builds qemu (won't deploy to silicon)
REM
REM   What it does:
REM     1. picks the make target based on first arg
REM     2. builds via WSL
REM     3. verifies the resulting kernel ELF entry point matches
REM        the expected address for the chosen platform
REM     4. commits + pushes
REM     5. prints the build number and embedded WARI-BUILD-TAG so
REM        the operator knows exactly what landed
REM ============================================================

REM Parse args. Two-arg form: <target> <msg>. One-arg: <msg> only.
REM Targets: vf2 (default, prod), vf2-debug (kdebug! on),
REM          vf2-trace (kdebug! + ktrace! on), qemu.
set "TARGET=vf2"
if /I "%~1"=="vf2" (
    set "TARGET=vf2"
    set "MSG=%~2"
) else if /I "%~1"=="vf2-debug" (
    set "TARGET=vf2-debug"
    set "MSG=%~2"
) else if /I "%~1"=="vf2-trace" (
    set "TARGET=vf2-trace"
    set "MSG=%~2"
) else if /I "%~1"=="qemu" (
    set "TARGET=qemu"
    set "MSG=%~2"
) else (
    set "MSG=%~1"
)
if "%MSG%"=="" set "MSG=wari deploy"

REM Map target -> make rule + expected ELF entry point.
if "%TARGET%"=="vf2" (
    set "MAKE_TARGET=kernel-vf2"
    set "EXPECTED_ENTRY=0x40200000"
) else if "%TARGET%"=="vf2-debug" (
    set "MAKE_TARGET=kernel-vf2-debug"
    set "EXPECTED_ENTRY=0x40200000"
) else if "%TARGET%"=="vf2-trace" (
    set "MAKE_TARGET=kernel-vf2-trace"
    set "EXPECTED_ENTRY=0x40200000"
) else (
    set "MAKE_TARGET=build"
    set "EXPECTED_ENTRY=0x80200000"
)

echo.
echo ============================================================
echo   wari deploy
echo     target:        %TARGET%
echo     make rule:     %MAKE_TARGET%
echo     expected ELF:  %EXPECTED_ENTRY%
echo     commit msg:    %MSG%
echo ============================================================
echo.

echo === Building via WSL ===
wsl -d Ubuntu --cd /mnt/c/projects/wari -- bash -lc "make %MAKE_TARGET%" || (
    echo.
    echo BUILD FAILED. Aborting deploy.
    exit /b 1
)

echo.
echo === Verifying build ===
wsl -d Ubuntu --cd /mnt/c/projects/wari -- bash scripts/verify-build.sh %EXPECTED_ENTRY% || (
    echo.
    echo VERIFY FAILED. Not committing.
    exit /b 1
)

echo.
echo === Committing + pushing from Windows ===
set /p BUILD=<.build_number
git add -A
git commit -m "Build %BUILD% [%TARGET%]: %MSG%" --allow-empty
git push origin HEAD

echo.
echo ============================================================
echo   DEPLOYED
echo     build:    %BUILD%
echo     target:   %TARGET%
echo ============================================================
if "%TARGET%"=="vf2" (
    echo   On the VF2:
    echo       wari go
    echo   Watch COM7 at 115200 baud
) else (
    echo   QEMU build — not for silicon. Run locally with:
    echo       wsl -d Ubuntu --cd /mnt/c/projects/wari -- make run
)
echo ============================================================
