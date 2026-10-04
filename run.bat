@echo off
setlocal
cd /d "%~dp0"

:: Check for administrative rights
net session >nul 2>&1
if %errorlevel% neq 0 (
    echo [INFO] Запрос прав Администратора для глобальных горячих клавиш LaneTheory в Dota 2...
    powershell -Command "Start-Process cmd.exe -ArgumentList '/c \"\"%~dpnx0\"\"' -Verb RunAs"
    exit /b
)

echo ===================================================
echo   LaneTheory
echo   F5: Сменить роль (Pos 1 -> Pos 5)
echo   F6: Только важные уведомления
echo   F7: Скрыть / показать всё
echo   F8: Настройки и интерактивный режим
echo ===================================================

set "EXE_PATH=%~dp0target\x86_64-pc-windows-msvc\release\lanetheory.exe"
if not exist "%EXE_PATH%" (
    set "EXE_PATH=%~dp0target\release\lanetheory.exe"
)

echo [INFO] Сборка актуальной release-версии...
set "CARGO_BUILD_JOBS=6"
cargo build --release
if %errorlevel% neq 0 (
    echo [ERROR] Сборка не удалась. Закройте старый overlay, если он ещё запущен.
    exit /b 1
)

start "" "%EXE_PATH%"
