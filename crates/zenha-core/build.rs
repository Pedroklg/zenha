use std::path::PathBuf;

fn main() {
    let root = PathBuf::from("third_party/rnnoise");
    println!("cargo:rerun-if-changed={}", root.display());

    let include_dir = root.join("include");
    let src_dir = root.join("src");
    let x86_dir = src_dir.join("x86");

    let target_arch = std::env::var("CARGO_CFG_TARGET_ARCH").unwrap_or_default();
    let target_env = std::env::var("CARGO_CFG_TARGET_ENV").unwrap_or_default();
    let is_msvc = target_env == "msvc";
    let is_x86_64 = target_arch == "x86_64";

    let configure = |build: &mut cc::Build| {
        build
            .include(&include_dir)
            .include(&src_dir)
            .include(&x86_dir)
            .define("RNNOISE_BUILD", None)
            .define("USE_WEIGHTS_FILE", None)
            .warnings(false);
        if is_msvc {
            build.define("_CRT_SECURE_NO_WARNINGS", None);
            if is_x86_64 {
                build.define("__SSE2__", None);
            }
        }
        if is_x86_64 {
            build.define("RNN_ENABLE_X86_RTCD", None);
            if !is_msvc {
                build.define("CPU_INFO_BY_C", None);
            }
        }
    };

    let mut common = cc::Build::new();
    configure(&mut common);
    for file in [
        "denoise.c",
        "rnn.c",
        "pitch.c",
        "kiss_fft.c",
        "celt_lpc.c",
        "nnet.c",
        "nnet_default.c",
        "parse_lpcnet_weights.c",
        "rnnoise_tables.c",
        "rnnoise_init_model.c",
    ] {
        common.file(src_dir.join(file));
    }
    if is_x86_64 {
        common.file(x86_dir.join("x86cpu.c"));
    }
    common.compile("zenha_rnnoise_common");

    if is_x86_64 {
        let mut rtcd_map = cc::Build::new();
        configure(&mut rtcd_map);
        rtcd_map
            .define("OPUS_X86_MAY_HAVE_SSE4_1", None)
            .define("OPUS_X86_MAY_HAVE_AVX2", None)
            .file(x86_dir.join("x86_dnn_map.c"));
        rtcd_map.compile("zenha_rnnoise_rtcd");

        let mut sse41 = cc::Build::new();
        configure(&mut sse41);
        if is_msvc {
            sse41.define("__SSE4_1__", None);
        } else {
            sse41.flag("-msse4.1");
        }
        sse41.file(x86_dir.join("nnet_sse4_1.c"));
        sse41.compile("zenha_rnnoise_sse41");

        let mut avx2 = cc::Build::new();
        configure(&mut avx2);
        if is_msvc {
            avx2.flag("/arch:AVX2")
                .define("__AVX2__", None)
                .define("__FMA__", None);
        } else {
            avx2.flag("-mavx2").flag("-mfma");
        }
        avx2.file(x86_dir.join("nnet_avx2.c"));
        avx2.compile("zenha_rnnoise_avx2");
    }
}
