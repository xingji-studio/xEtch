@echo off
setlocal
if defined XETCH_PYTHON goto custom
where py >nul 2>nul
if not errorlevel 1 goto launcher
where python >nul 2>nul
if not errorlevel 1 goto python
echo Python 3.12+ is required for building. Install Python or set XETCH_PYTHON.
exit /b 1
:custom
"%XETCH_PYTHON%" "%~dp0scripts\xetch_build.py" %*
exit /b %ERRORLEVEL%
:launcher
py -3 "%~dp0scripts\xetch_build.py" %*
exit /b %ERRORLEVEL%
:python
python "%~dp0scripts\xetch_build.py" %*
exit /b %ERRORLEVEL%
