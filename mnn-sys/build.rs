use ::tap::*;
use anyhow::*;
use std::path::{Path, PathBuf};
const VENDOR: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/vendor");
const MANIFEST_DIR: &str = env!("CARGO_MANIFEST_DIR");

fn ensure_vendor_exists(vendor: impl AsRef<Path>) -> Result<()> {
    if vendor.as_ref().read_dir()?.flatten().count() == 0 {
        anyhow::bail!("Vendor not found maybe you need to run \"git submodule update --init\"")
    }
    Ok(())
}

fn main() -> Result<()> {
    println!("cargo:rerun-if-changed=build.rs");
    let out_dir = PathBuf::from(std::env::var("OUT_DIR")?);
    ensure_vendor_exists(VENDOR)?;

    let vendor = out_dir.join("vendor");
    if !vendor.exists() {
        fs_extra::dir::copy(
            VENDOR,
            &vendor,
            &fs_extra::dir::CopyOptions::new()
                .overwrite(true)
                .copy_inside(true),
        )
        .context("Failed to copy vendor")?;
        try_patch_file(
            "patches/typedef_template.patch",
            &vendor.join("include").join("MNN").join("Interpreter.hpp"),
        )
        .context("Failed to patch vendor")?;
    }

    mnn_c_build(PathBuf::from(MANIFEST_DIR).join("mnn_c"), &vendor)
        .with_context(|| "Failed to build mnn_c")?;
    mnn_c_bindgen(&vendor, &out_dir).with_context(|| "Failed to generate mnn_c bindings")?;
    let install_dir = out_dir.join("mnn-install");
    build_cmake(&vendor, &install_dir)?;
    println!("cargo:include={vendor}/include", vendor = vendor.display());
    if cfg!(target_os = "macos") {
        println!("cargo:rustc-link-lib=framework=Foundation");
        #[cfg(feature = "metal")]
        println!("cargo:rustc-link-lib=framework=CoreGraphics");
        #[cfg(feature = "metal")]
        println!("cargo:rustc-link-lib=framework=Metal");
        #[cfg(feature = "coreml")]
        println!("cargo:rustc-link-lib=framework=CoreML");
        #[cfg(feature = "coreml")]
        println!("cargo:rustc-link-lib=framework=CoreVideo");
        #[cfg(feature = "opencl")]
        println!("cargo:rustc-link-lib=framework=OpenCL");
    }
    println!(
        "cargo:rustc-link-search=native={}",
        install_dir.join("lib").display()
    );
    println!("cargo:rustc-link-lib=static=MNN");
    Ok(())
}

pub fn mnn_c_bindgen(vendor: impl AsRef<Path>, out: impl AsRef<Path>) -> Result<()> {
    let vendor = vendor.as_ref();
    let mnn_c = PathBuf::from(MANIFEST_DIR).join("mnn_c");
    mnn_c.read_dir()?.flatten().for_each(|e| {
        rerun_if_changed(e.path());
    });
    const HEADERS: &[&str] = &[
        "ErrorCode_c.h",
        "Interpreter_c.h",
        "Tensor_c.h",
        "Backend_c.h",
        "Schedule_c.h",
    ];

    let bindings = bindgen::Builder::default()
        .pipe(|builder| {
            #[cfg(feature = "vulkan")]
            let builder = builder.clang_arg("-DMNN_VULKAN=1");
            #[cfg(feature = "metal")]
            let builder = builder.clang_arg("-DMNN_METAL=1");
            #[cfg(feature = "coreml")]
            let builder = builder.clang_arg("-DMNN_COREML=1");
            #[cfg(feature = "opencl")]
            let builder = builder.clang_arg("-DMNN_OPENCL=1");
            // #[cfg(feature = "vulkan")]
            // let builder = builder.clang_args(
            //     vulkan_includes(vendor)
            //         .iter()
            //         .map(|p| format!("-I{}", p.display())),
            // );
            builder
        })
        .detect_include_paths(true)
        .clang_arg(format!("-I{}", vendor.join("include").to_string_lossy()))
        .pipe(|generator| {
            HEADERS.iter().fold(generator, |gen, header| {
                gen.header(mnn_c.join(header).to_string_lossy())
            })
        })
        .rustified_enum("MemoryMode")
        .rustified_enum("PowerMode")
        .rustified_enum("PrecisionMode")
        .rustified_enum("SessionMode")
        .rustified_enum("DimensionType")
        .rustified_enum("HandleDataType")
        .rustified_enum("MapType")
        .rustified_enum("halide_type_code_t")
        .rustified_enum("ErrorCode")
        .rustified_enum("MNNGpuMode")
        .rustified_enum("MNNForwardType")
        .rustified_enum("RuntimeStatus")
        .no_copy("CString")
        .generate_cstr(true)
        .generate_inline_functions(true)
        .size_t_is_usize(true)
        .generate()?;
    bindings.write_to_file(out.as_ref().join("mnn_c.rs"))?;
    Ok(())
}

pub fn mnn_c_build(path: impl AsRef<Path>, vendor: impl AsRef<Path>) -> Result<()> {
    let mnn_c = path.as_ref();
    let files = mnn_c.read_dir()?.flatten().map(|e| e.path()).filter(|e| {
        e.extension() == Some(std::ffi::OsStr::new("cpp"))
            || e.extension() == Some(std::ffi::OsStr::new("c"))
    });
    let vendor = vendor.as_ref();
    cc::Build::new()
        .include(vendor.join("include"))
        // .includes(vulkan_includes(vendor))
        .pipe(|config| {
            #[cfg(feature = "vulkan")]
            config.define("MNN_VULKAN", "1");
            #[cfg(feature = "metal")]
            config.define("MNN_METAL", "1");
            #[cfg(feature = "coreml")]
            config.define("MNN_COREML", "1");
            #[cfg(feature = "opencl")]
            config.define("MNN_OPENCL", "ON");
            config
        })
        .cpp(true)
        .static_flag(true)
        .static_crt(true)
        .files(files)
        .std("c++14")
        .try_compile("mnn_c")
        .context("Failed to compile mnn_c library")?;
    Ok(())
}

pub fn build_cmake(path: impl AsRef<Path>, install: impl AsRef<Path>) -> Result<()> {
    let threads = std::thread::available_parallelism()?;
    cmake::Config::new(path)
        .parallel(threads.get() as u8)
        .cxxflag("-std=c++14")
        .define("MNN_BUILD_SHARED_LIBS", "OFF")
        .define("MNN_SEP_BUILD", "OFF")
        .define("MNN_PORTABLE_BUILD", "ON")
        .define("MNN_USE_SYSTEM_LIB", "OFF")
        .define("MNN_BUILD_CONVERTER", "OFF")
        .define("MNN_BUILD_TOOLS", "OFF")
        .define("CMAKE_INSTALL_PREFIX", install.as_ref())
        .define("MNN_WIN_RUNTIME_MT", "ON")
        // https://github.com/rust-lang/rust/issues/39016
        // https://github.com/rust-lang/cc-rs/pull/717
        // .define("CMAKE_BUILD_TYPE", "Release")
        .pipe(|config| {
            #[cfg(feature = "vulkan")]
            config.define("MNN_VULKAN", "ON");
            #[cfg(feature = "metal")]
            config.define("MNN_METAL", "ON");
            #[cfg(feature = "coreml")]
            config.define("MNN_COREML", "ON");
            #[cfg(feature = "opencl")]
            config.define("MNN_OPENCL", "ON");
            config
        })
        .build();
    Ok(())
}

pub fn try_patch_file(patch: impl AsRef<Path>, file: impl AsRef<Path>) -> Result<()> {
    let patch = dunce::canonicalize(patch)?;
    rerun_if_changed(&patch);
    let patch = std::fs::read_to_string(&patch)?;
    let patch = diffy::Patch::from_str(&patch)?;
    // let vendor = vendor.as_ref();
    // let interpreter_path = vendor.join("include").join("MNN").join("Interpreter.hpp");
    let file_path = file.as_ref();
    let file = std::fs::read_to_string(&file_path).context("Failed to read input file")?;
    let patched_file =
        diffy::apply(&file, &patch).context("Failed to apply patches using diffy")?;
    std::fs::write(file_path, patched_file)?;
    Ok(())
}

pub fn rerun_if_changed(path: impl AsRef<Path>) {
    println!("cargo:rerun-if-changed={}", path.as_ref().display());
}

pub fn vulkan_includes(vendor: impl AsRef<Path>) -> Vec<PathBuf> {
    let vendor = vendor.as_ref();
    let vulkan_dir = vendor.join("source/backend/vulkan");
    if cfg!(feature = "vulkan") {
        vec![
            vulkan_dir.clone(),
            vulkan_dir.join("runtime"),
            vulkan_dir.join("component"),
            // IDK If the order is important but the cmake file does it like this
            vulkan_dir.join("buffer/execution"),
            vulkan_dir.join("buffer/backend"),
            vulkan_dir.join("buffer"),
            vulkan_dir.join("buffer/shaders"),
            // vulkan_dir.join("image/execution"),
            // vulkan_dir.join("image/backend"),
            // vulkan_dir.join("image"),
            // vulkan_dir.join("image/shaders"),
            vendor.join("schema/current"),
            vendor.join("3rd_party/flatbuffers/include"),
            vendor.join("source"),
        ]
    } else {
        vec![]
    }
}
