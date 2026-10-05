@echo off
rem Run EverOS in QEMU on Windows. Put this file next to everos.iso.
rem Files saved in EverOS are kept in everos-disk.vhd next to it, which is
rem made on the first run. Windows opens it too: double-click the .vhd
rem while EverOS is not running, and eject it before starting EverOS again.
setlocal
cd /d "%~dp0"
set QEMU=qemu-system-x86_64.exe
set QEMU_IMG=qemu-img.exe
where %QEMU% >nul 2>nul || set QEMU="C:\Program Files\qemu\qemu-system-x86_64.exe"
where %QEMU_IMG% >nul 2>nul || set QEMU_IMG="C:\Program Files\qemu\qemu-img.exe"
if not exist everos-disk.vhd %QEMU_IMG% create -f vpc -o subformat=fixed everos-disk.vhd 128M
rem Hardware acceleration (Windows Hypervisor Platform) makes RyzikOS
rem several times faster; without it QEMU falls back to plain emulation.
set ACCEL=-accel whpx,kernel-irqchip=off -accel tcg
rem Shared clipboard: RyzikOS's second serial port talks to
rem ryzikos-clipboard.ps1 (keep it next to this file), which copies text
rem between the Windows clipboard and RyzikOS's.
set CLIP=
if exist "%~dp0ryzikos-clipboard.ps1" (
    set CLIP=-chardev socket,id=clip,host=127.0.0.1,port=45577,server=on,wait=off -serial chardev:clip
    start "" /min powershell -NoProfile -STA -ExecutionPolicy Bypass -WindowStyle Hidden -File "%~dp0ryzikos-clipboard.ps1"
)
set DISK=
if exist everos-disk.vhd set DISK=-drive file=everos-disk.vhd,format=vpc,if=ide,index=0,media=disk
%QEMU% %ACCEL% -cdrom everos.iso -boot d -m 512M -serial stdio %CLIP% -rtc base=localtime -nic user,model=e1000 -audiodev dsound,id=snd0 -device ES1370,audiodev=snd0 %DISK%
