@echo off
rem Assemble duscape inside DOS. Run from the repository root (mounted as a drive), with
rem FASM.EXE and CWSDPMI.EXE in TARGET\DOS (`make dos-tools` puts them there).
TARGET\DOS\CWSDPMI -p
TARGET\DOS\FASM VIEWERS\DOS\DUSCAPE.ASM TARGET\DOS\DUSCAPE.EXE
