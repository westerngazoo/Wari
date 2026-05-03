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
set "TARGET=vf2"
if /I "%~1"=="vf2" (
    set "TARGET=vf2"
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
REM Read .build_number, embedded tag, and ELF entry point. The Python
REM one-liner runs in WSL because it is the cleanest way to parse the
REM ELF header on this system; failure aborts the deploy.
wsl -d Ubuntu --cd /mnt/c/projects/wari -- bash -lc ^
  "test -s build/wari.bin || { echo 'build/wari.bin missing or empty'; exit 1; }; \
   BUILD=$(cat .build_number); \
   TAG=$(strings build/wari.bin | grep WARI-BUILD-TAG- | head -1); \
   ENTRY=$(python3 -c 'import struct; f=open(\"target/riscv64gc-unknown-none-elf/release/wari\",\"rb\"); d=f.read(0x100); print(hex(struct.unpack(\"<Q\", d[0x18:0x20])[0]))'); \
   echo \"  .build_number:          $BUILD\"; \
   echo \"  embedded tag:           $TAG\"; \
   echo \"  kernel ELF entry:       $ENTRY\"; \
   echo \"  expected entry:         %EXPECTED_ENTRY%\"; \
   if [ \"$ENTRY\" != \"%EXPECTED_ENTRY%\" ]; then \
     echo ''; \
     echo \"VERIFY FAILED: entry point $ENTRY does not match expected %EXPECTED_ENTRY%\"; \
     echo \"               (target=%TARGET% — wrong make rule produced this binary?)\"; \
     exit 1; \
   fi" || (
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
