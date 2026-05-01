@echo off
setlocal enabledelayedexpansion

if "%~1"=="" (
    set "MSG=wari deploy"
) else (
    set "MSG=%~1"
)

echo === Building VF2 kernel via WSL ===
wsl -d Ubuntu --cd /mnt/c/projects/wari -- bash -lc "make kernel-vf2" || exit /b 1

echo === Committing + pushing from Windows ===
set /p BUILD=<.build_number
git add -A
git commit -m "Build %BUILD%: %MSG%" --allow-empty
git push origin HEAD

echo.
echo =========================================
echo   DEPLOYED: build %BUILD%
echo =========================================
echo   On the VF2:
echo       wari go
echo   Watch COM7 at 115200 baud
echo =========================================
