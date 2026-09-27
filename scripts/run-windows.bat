@echo off
rem Run RyzikOS in QEMU on Windows. Put this file next to ryzikos.iso.
rem Files saved in RyzikOS are kept in ryzikos-disk.vhd next to it, which is
rem made on the first run. Windows opens it too: double-click the .vhd
rem while RyzikOS is not running, and eject it before starting RyzikOS again.
setlocal
cd /d "%~dp0"
set QEMU=qemu-system-x86_64.exe
set QEMU_IMG=qemu-img.exe
where %QEMU% >nul 2>nul || set QEMU="C:\Program Files\qemu\qemu-system-x86_64.exe"
where %QEMU_IMG% >nul 2>nul || set QEMU_IMG="C:\Program Files\qemu\qemu-img.exe"
if not exist ryzikos-disk.vhd %QEMU_IMG% create -f vpc -o subformat=fixed ryzikos-disk.vhd 128M
set DISK=
if exist ryzikos-disk.vhd set DISK=-drive file=ryzikos-disk.vhd,format=vpc,if=ide,index=0,media=disk
%QEMU% -cdrom ryzikos.iso -boot d -m 512M -serial stdio -rtc base=localtime -nic user,model=e1000 %DISK%
