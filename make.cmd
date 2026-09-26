@echo off
rem `make` for Windows without make: see make.ps1, which reads the Makefile beside it.
powershell -NoProfile -ExecutionPolicy Bypass -File "%~dp0make.ps1" %*
exit /b %ERRORLEVEL%
