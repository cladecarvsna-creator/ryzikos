# RyzikOS build: nasm (boot code) + cargo (Rust kernel) -> ld -> GRUB ISO.

BUILD      := build
KERNEL     := $(BUILD)/kernel.bin
ISO        := $(BUILD)/everos.iso
# the hard disk for `make run`: files saved in EverOS stay here
DISK       := $(BUILD)/disk.img
RUST_LIB   := kernel/target/x86_64-unknown-none/release/libeveros_kernel.a
ASM_SRC    := $(wildcard boot/*.asm)
ASM_OBJ    := $(patsubst boot/%.asm,$(BUILD)/boot/%.o,$(ASM_SRC))
QEMU       := qemu-system-x86_64
# The release number, the N in 1.0.N; GitHub sets it, 0 for your own build.
# The kernel reads it at compile time, for Settings > Update.
RYZIKOS_BUILD ?= 0
export RYZIKOS_BUILD

.PHONY: all iso run test clean kernel-lib FORCE

all: $(ISO)

iso: $(ISO)

$(BUILD)/boot/%.o: boot/%.asm
	@mkdir -p $(dir $@)
	nasm -f elf64 $< -o $@

kernel-lib: $(RUST_LIB)

# cargo decides what to rebuild; having the recipe here (not on a phony
# target) makes make look at the library's new time and link again
$(RUST_LIB): FORCE
	cd kernel && cargo build --release

FORCE:

# --strip-debug: the kernel never reads its debug info, and a smaller
# kernel.gz loads faster from a CD and downloads faster as an update
$(KERNEL): $(ASM_OBJ) $(RUST_LIB) linker.ld
	ld -n --gc-sections --strip-debug -z noexecstack --no-warn-rwx-segments -T linker.ld -o $@ $(ASM_OBJ) $(RUST_LIB)
	grub-file --is-x86-multiboot2 $@

# GRUB for a hard disk RyzikOS is installed on: core.img goes in the
# sectors before the first partition and holds every module it needs.
GRUB_PC    := /usr/lib/grub/i386-pc
DISK_GRUB  := biosdisk part_msdos fat normal multiboot2 gzio all_video test loadenv configfile echo

MEDIA      := iso/media/Pictures iso/media/Videos $(wildcard programs/*)

# The disc also carries sample photos and a video (made by
# scripts/gen-media.py) and the programs, which RyzikOS reads at /Disc.
$(ISO): $(KERNEL) iso/boot/grub/grub.cfg $(MEDIA)
	@mkdir -p $(BUILD)/iso/boot/grub
	rm -f $(BUILD)/iso/boot/kernel.bin
	gzip -9 -n -c $(KERNEL) > $(BUILD)/iso/boot/kernel.gz
	@mkdir -p $(BUILD)/iso/boot/grub/ryzikos
	grub-mkimage -O i386-pc -o $(BUILD)/iso/boot/grub/ryzikos/core.img -p '(,msdos1)/boot/grub' $(DISK_GRUB)
	cp $(GRUB_PC)/boot.img $(BUILD)/iso/boot/grub/ryzikos/boot.img
	sed 's/@BUILD@/$(RYZIKOS_BUILD)/' iso/boot/grub/grub.cfg > $(BUILD)/iso/boot/grub/grub.cfg
	rm -rf $(BUILD)/iso/Pictures $(BUILD)/iso/Videos $(BUILD)/iso/Programs
	cp -r iso/media/. $(BUILD)/iso/
	mkdir -p $(BUILD)/iso/Programs
	cp programs/*.rzapp programs/catalog.txt $(BUILD)/iso/Programs/
	grub-mkrescue -o $@ $(BUILD)/iso -- -volid RYZIKOS_1_0 2> /dev/null

# A blank disk; EverOS formats it as FAT32 on first boot. Read it with
# mtools: mdir -i build/disk.img@@1M ::/Users/root
$(DISK):
	@mkdir -p $(BUILD)
	truncate -s 128M $@

# Boot EverOS in a QEMU window. Serial output goes to the terminal, and
# the taskbar clock shows local time.
run: $(ISO) $(DISK)
	$(QEMU) -cdrom $(ISO) -boot d -m 512M -serial stdio -rtc base=localtime \
		-drive file=$(DISK),format=raw,if=ide,index=0,media=disk \
		-nic user,model=e1000 -device ES1370

# Boot headless and check that the kernel reached Rust code.
test: $(ISO)
	bash scripts/boot-test.sh $(ISO)

# keeps build/disk.img, so saved files survive a clean
clean:
	rm -rf $(BUILD)/boot $(BUILD)/iso $(KERNEL) $(ISO)
	cd kernel && cargo clean
