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

$(KERNEL): $(ASM_OBJ) $(RUST_LIB) linker.ld
	ld -n --gc-sections -z noexecstack --no-warn-rwx-segments -T linker.ld -o $@ $(ASM_OBJ) $(RUST_LIB)
	grub-file --is-x86-multiboot2 $@

MEDIA      := iso/media/Pictures iso/media/Videos $(wildcard programs/*)

# The disc also carries sample photos and a video (made by
# scripts/gen-media.py) and the programs, which RyzikOS reads at /Disc.
$(ISO): $(KERNEL) iso/boot/grub/grub.cfg $(MEDIA)
	@mkdir -p $(BUILD)/iso/boot/grub
	cp $(KERNEL) $(BUILD)/iso/boot/kernel.bin
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
