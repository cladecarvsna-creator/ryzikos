//! Compile the QuickJS JavaScript engine (quickjs/) and its small C
//! library into the kernel. Needs clang.

fn main() {
    let files = [
        "quickjs/quickjs.c",
        "quickjs/libregexp.c",
        "quickjs/libunicode.c",
        "quickjs/cutils.c",
        "quickjs/dtoa.c",
        "quickjs/glue.c",
        "quickjs/libc/libc.c",
    ];
    for f in files {
        println!("cargo:rerun-if-changed={}", f);
    }
    println!("cargo:rerun-if-changed=quickjs/libc/include");
    let version = std::fs::read_to_string("quickjs/VERSION").unwrap();
    cc::Build::new()
        .compiler("clang")
        .files(files)
        .include("quickjs/libc/include")
        .define("__everos__", None)
        .define("CONFIG_VERSION", format!("\"{}\"", version.trim()).as_str())
        .define("NDEBUG", None)
        // freestanding kernel code: no host headers, no red zone (interrupts
        // share the stack), code linked at a fixed low address
        .flag("--target=x86_64-unknown-none-elf")
        .flag("-ffreestanding")
        .flag("-fno-math-errno")
        .flag("-nostdlibinc")
        .flag("-mno-red-zone")
        .flag("-mcmodel=small")
        .flag("-fno-stack-protector")
        .flag("-fno-asynchronous-unwind-tables")
        .flag("-w")
        .pic(false)
        .opt_level(2)
        .warnings(false)
        .compile("quickjs");
}
