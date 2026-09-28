#!/usr/bin/env bash
# Boot the ISO in QEMU without a display, wait for the kernel's serial
# message, sign in as root on the lock screen, wait for the desktop, then
# type commands on the emulated PS/2 keyboard and check that the shell in
# the terminal window ran them.
# A small web server on the host checks the network card, TCP/IP and
# HTTP: the guest reaches the host at 10.0.2.2 through QEMU's user network.
# The ISO is a live CD: it leaves the blank disk alone until `install`
# puts RyzikOS on it. Then QEMU starts from the hard disk alone, Welcome
# opens, the shell writes a file, and after starting again the file is
# still there.
# Exits 0 if everything works, 1 otherwise.
set -u

iso="${1:-build/everos.iso}"
dir="$(mktemp -d)"
log="$dir/serial.log"
monitor="$dir/monitor.sock"
disk="$dir/disk.img"
qemu=""
trap 'kill $qemu "$web" 2> /dev/null; rm -rf "$dir"' EXIT
truncate -s 64M "$disk"

mkdir "$dir/www"
echo '<html><head><title>EverOS test page</title></head><body><h1>It works</h1><a href="/x">x</a></body></html>' \
    > "$dir/www/index.html"
echo '<title>Test program</title><p>hello</p>' > "$dir/www/test.rzapp"
python3 -m http.server 8123 --bind 127.0.0.1 --directory "$dir/www" > /dev/null 2>&1 &
web=$!

# start QEMU with the disk, from the live CD or (with "disk") from the
# hard disk without a CD; the serial log starts empty
boot() {
    rm -f "$log" "$monitor"
    local media=(-cdrom "$iso" -boot d)
    if [ "${1:-}" = disk ]; then
        media=(-boot c)
    fi
    timeout 90 qemu-system-x86_64 "${media[@]}" -m 512M -display none \
        -serial "file:$log" -monitor "unix:$monitor,server,nowait" -no-reboot \
        -drive "file=$disk,format=raw,if=ide,index=0,media=disk" \
        -nic user,model=e1000 2> /dev/null &
    qemu=$!
}
boot

# wait for a line starting with $1 in the serial log
wait_for() {
    for _ in $(seq 1 60); do
        if grep -q "^$1" "$log" 2> /dev/null; then
            return 0
        fi
        sleep 0.5
    done
    return 1
}

fail() {
    echo "boot test failed: $1, serial output was:"
    cat "$log"
    exit 1
}

wait_for "EverOS: kernel started" || fail "the kernel did not start"
echo "kernel started"

# press keys on the emulated keyboard through the QEMU monitor
type_keys() {
    python3 - "$monitor" "$@" << 'PY'
import socket, sys, time
s = socket.socket(socket.AF_UNIX)
s.connect(sys.argv[1])
for key in sys.argv[2:]:
    s.sendall(f"sendkey {key}\n".encode())
    time.sleep(0.1)
PY
}

sign_in() {
    wait_for "login: lock screen" || fail "the lock screen did not show"
    type_keys ret
    wait_for "login: password prompt" || fail "the sign-in panel did not open"
    # root has no password at first
    type_keys ret
    wait_for "login: signed in as root" || fail "could not sign in as root"
    echo "signed in as root"
}

wait_for "fs: live CD" || fail "the disc did not start as a live CD"
wait_for "fs: files are kept in memory only" || fail "the live CD did not keep files in memory"
grep -q "formatted" "$log" && fail "the live CD formatted the disk"
echo "live CD started, the disk is left alone"
wait_for "fs: disc in the drive: RYZIKOS_1_0" || fail "the CD drive or the disc was not found"
echo "the RyzikOS disc is readable"
sign_in

wait_for "desktop: opened Terminal" || fail "the desktop did not start"
echo "desktop started"
wait_for "icons: loaded 23 pictures" || fail "the app icons did not load"
echo "app icons loaded"

type_keys e c h o spc k e y b o a r d minus o k ret
wait_for "keyboard-ok" || fail "the shell did not answer typed input"
echo "keyboard input works"

type_keys f e t c h spc 1 0 dot 0 dot 2 dot 2 shift-semicolon 8 1 2 3 slash ret
wait_for 'fetch: "EverOS test page"' || fail "the network test page did not load"
echo "network and HTTP work"

# the browser loads pages on a fiber, in the background
type_keys b r o w s e r spc 1 0 dot 0 dot 2 dot 2 shift-semicolon 8 1 2 3 slash ret
wait_for "browser: showing page" || fail "the browser did not show the test page"
echo "the browser loads pages in the background"
# back to the terminal through the taskbar search
type_keys meta_l-s
wait_for "search: indexed" || fail "the taskbar search did not open"
type_keys t e r m ret
sleep 1

# the browser downloads a program into Programs
type_keys b r o w s e r spc 1 0 dot 0 dot 2 dot 2 shift-semicolon 8 1 2 3 slash t e s t dot r z a p p ret
wait_for "browser: downloaded test.rzapp" || fail "the browser did not download the program"
echo "the browser downloads programs"
type_keys meta_l-s
type_keys t e r m ret
sleep 1

# Video Player plays the demo video from the disc
type_keys v i d e o spc slash shift-d i s c slash shift-v i d e o s slash shift-r y z i k shift-o shift-s spc shift-d e m o dot a v i ret
wait_for "video: opened RyzikOS Demo.avi, Motion JPEG, 250 frames" || fail "Video Player could not open the video on the disc"
echo "Video Player plays video from the disc"
type_keys meta_l-s
type_keys t e r m ret
sleep 1
# and the same video as an H.264 MP4, read from the disc as it plays
type_keys v i d e o spc slash shift-d i s c slash shift-v i d e o s slash shift-r y z i k shift-o shift-s spc shift-d e m o dot m p 4 ret
wait_for "video: opened RyzikOS Demo.mp4, H.264, 250 pictures, 640x360" || fail "Video Player could not open the MP4 video on the disc"
wait_for "video: finished RyzikOS Demo.mp4" || fail "the MP4 video did not play to its end"
echo "Video Player plays H.264 MP4 video"
type_keys meta_l-s
type_keys t e r m ret
sleep 1

# Telegram's cryptography, against known answers
type_keys t e l e g r a m spc s e l f t e s t ret
wait_for "telegram: self-test ok" || fail "the Telegram self-test failed"
echo "Telegram's cryptography works"

# Archiver: zip a file, unpack it into a new folder, read it back, then
# open the archive in the Archiver window
type_keys e c h o spc r o u n d t r i p minus o k spc shift-dot spc z dot t x t ret
type_keys z i p spc t dot z i p spc z dot t x t ret
wait_for "zip: packed 1 into t.zip" || fail "zip could not pack a file"
type_keys u n z i p spc t dot z i p ret
wait_for "unzip: unpacked 1 files to /Users/root/t" || fail "unzip could not unpack the archive"
type_keys c a t spc t slash z dot t x t ret
wait_for "roundtrip-ok" || fail "the unpacked file was not the same"
echo "zip and unzip work"
type_keys a r c h i v e r spc t dot z i p ret
wait_for "desktop: opened Archiver" || fail "the shell could not open Archiver"
wait_for "archiver: opened t.zip, 1 entry" || fail "Archiver could not open the archive"
echo "Archiver opens archives"
type_keys meta_l-s
type_keys t e r m ret
sleep 1

# PrintScreen, then Enter for the whole screen: saved and copied
type_keys print
wait_for "screenshot: pick an area" || fail "PrintScreen did not start a screenshot"
type_keys ret
wait_for "screenshot: saved /Users/root/Pictures/Screenshots/Screenshot.png" || fail "the screenshot was not saved"
echo "PrintScreen saves a screenshot"

quit_qemu() {
    python3 - "$monitor" << 'PY'
import socket, sys
s = socket.socket(socket.AF_UNIX)
s.connect(sys.argv[1])
s.sendall(b"quit\n")
PY
    wait "$qemu" 2> /dev/null
}

# install on the blank disk, then again next to the files on it
type_keys i n s t a l l spc e r a s e spc 1 ret
wait_for "install: RyzikOS installed on /Disk 1" || fail "the installer could not install on the blank disk"
echo "installed on the blank disk"
type_keys i n s t a l l spc k e e p spc 1 ret
wait_for "fs: Disk 1 has FAT32" || true
sleep 3
[ "$(grep -c "^install: RyzikOS installed" "$log")" = 2 ] || fail "installing next to the files failed"
echo "installed again, keeping the files"
quit_qemu

# the installed system starts from the hard disk, without the CD
boot disk
wait_for "EverOS: kernel started" || fail "the installed RyzikOS did not start from the hard disk"
wait_for "fs: mounted FAT32 disk" || fail "the installed RyzikOS did not mount its disk"
echo "the installed RyzikOS starts from the hard disk"
sign_in
wait_for "desktop: opened Welcome" || fail "Welcome did not open after installing"
echo "Welcome opens after the first sign-in"
type_keys esc
sleep 1

# write a file on the disk, in root's home folder, and open it in
# Notepad (which then has the keyboard)
type_keys e c h o spc s a v e d minus o k spc shift-dot spc s a v e d dot t x t ret
type_keys n o t e p a d spc s a v e d dot t x t ret
wait_for "desktop: opened Text Editor" || fail "the shell could not open the Text Editor"
echo "file written, apps open from the shell"

# start again from the same disk: the file must still be there
quit_qemu
if command -v mtype > /dev/null; then
    mtype -i "$disk@@1M" ::/Users/root/saved.txt | grep -q "saved-ok" \
        || fail "mtools could not read the file EverOS wrote"
    echo "mtools reads the file EverOS wrote"
fi
if command -v fsck.fat > /dev/null; then
    part="$dir/part.img"
    dd if="$disk" of="$part" bs=512 skip=2048 status=none
    fsck.fat -n "$part" > "$dir/fsck.log" 2>&1 || { cat "$dir/fsck.log"; fail "fsck.fat found errors"; }
    echo "fsck.fat finds no errors"
fi

boot disk
wait_for "fs: mounted FAT32 disk" || fail "the disk was not mounted again"
sign_in
wait_for "desktop: opened Terminal" || fail "the desktop did not start again"
type_keys c a t spc s a v e d dot t x t ret
wait_for "saved-ok" || fail "the file was gone after restarting"
echo "files survive a restart"
type_keys s e t t i n g s ret
wait_for "desktop: opened Settings" || fail "the shell could not open Settings"
echo "Settings opens"

# search from the taskbar: Win+S, type, Enter opens the best match
type_keys meta_l-s
wait_for "search: indexed" || fail "the taskbar search did not open"
type_keys c a l c ret
wait_for "desktop: opened Calculator" || fail "search did not open Calculator"
echo "taskbar search works"

# virtual desktops: Win+Ctrl+D makes one, Win+Ctrl+Left goes back
type_keys ctrl-meta_l-d
wait_for "desktops: switched to Desktop 2" || fail "Win+Ctrl+D did not make a new desktop"
type_keys ctrl-meta_l-left
wait_for "desktops: switched to Desktop 1" || fail "Win+Ctrl+Left did not switch back"
type_keys meta_l-tab
wait_for "desktops: task view" || fail "Win+Tab did not open Task View"
echo "virtual desktops and Task View work"

# the blue screen: a panic saves a report on the disk and restarts (QEMU
# quits on the restart), and the next sign-in says why it restarted
type_keys esc
sleep 1
type_keys meta_l-s
type_keys t e r m ret
sleep 1
type_keys p a n i c ret
wait_for "bsod: PANIC_REQUESTED" || fail "panic did not show the blue screen"
wait_for "crash: report saved to the disk" || fail "the blue screen did not save the crash report"
type_keys ret
wait_for "crash: restarting" || fail "a key did not restart from the blue screen"
wait "$qemu" 2> /dev/null
echo "panic shows the blue screen and saves a report"
boot disk
wait_for "crash: the last run stopped with PANIC_REQUESTED" || fail "the crash report was not found after restarting"
sign_in
wait_for "crash: showing the report, PANIC_REQUESTED" || fail "the crash report window did not open"
echo "after the restart, the crash report explains what happened"
echo "boot test passed"
