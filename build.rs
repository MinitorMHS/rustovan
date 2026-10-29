use std::{env, path::PathBuf, process::Command};

fn main() {
    println!("cargo:rerun-if-changed=cuda/pbkdf2.cu");
    println!("cargo:rerun-if-env-changed=TON_VANITY_AGE_RECIPIENT");
    for variable in ["CUDA_HOME", "CUDA_PATH", "TON_CUDA_ARCH"] {
        println!("cargo:rerun-if-env-changed={variable}");
    }

    if env::var_os("CARGO_FEATURE_SEALED_OUTPUT").is_some() {
        let recipient = env::var("TON_VANITY_AGE_RECIPIENT").unwrap_or_else(|_| {
            panic!(
                "sealed-output requires TON_VANITY_AGE_RECIPIENT to contain an age public recipient key"
            )
        });
        if recipient.starts_with("age1pq1") {
            panic!(
                "standard age1pq1 software recipients are not supported by the pinned Rust age library; use an age1tagpq1 hardware recipient or classic age1 X25519 recipient"
            );
        }
        let valid_classic = recipient.len() == 62 && recipient.starts_with("age1");
        let valid_tagged_pq = recipient.starts_with("age1tagpq1") && recipient.len() > 100;
        if !(valid_classic || valid_tagged_pq)
            || !recipient
                .bytes()
                .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit())
        {
            panic!(
                "TON_VANITY_AGE_RECIPIENT must be a lowercase age X25519 (age1..., 62 chars) or tagged post-quantum hardware recipient (age1tagpq1...)"
            );
        }
        println!("cargo:rustc-env=TON_VANITY_AGE_RECIPIENT={recipient}");
    }

    if env::var_os("CARGO_FEATURE_CUDA").is_none() {
        return;
    }

    let target = env::var("TARGET").expect("Cargo must set TARGET");
    let is_windows = target.contains("windows");
    if is_windows && !target.contains("msvc") {
        panic!("the CUDA backend currently supports the MSVC Windows target, not {target}");
    }
    let cuda_home = env::var("CUDA_HOME")
        .or_else(|_| env::var("CUDA_PATH"))
        .unwrap_or_else(|_| {
            if is_windows {
                panic!(
                    "CUDA feature requested on Windows; set CUDA_PATH to the CUDA Toolkit directory"
                );
            }
            "/usr/local/cuda".into()
        });
    let cuda_home = PathBuf::from(cuda_home);
    let nvcc_name = if is_windows { "nvcc.exe" } else { "nvcc" };
    let nvcc = cuda_home.join("bin").join(nvcc_name);
    if !nvcc.exists() {
        panic!(
            "CUDA feature requested but nvcc was not found at {}. Set CUDA_HOME/CUDA_PATH to the Toolkit directory.",
            nvcc.display()
        );
    }

    let out = PathBuf::from(env::var_os("OUT_DIR").expect("Cargo must set OUT_DIR"));
    let obj = out.join(if is_windows {
        "ton_vanity_cuda.obj"
    } else {
        "ton_vanity_cuda.o"
    });
    let archive = out.join(if is_windows {
        "ton_vanity_cuda.lib"
    } else {
        "libton_vanity_cuda.a"
    });
    let mut compile = Command::new(&nvcc);
    compile
        .arg("-O3")
        .arg("--std=c++17")
        .arg("-c")
        .arg("cuda/pbkdf2.cu")
        .arg("-o")
        .arg(&obj);
    if let Ok(arch) = env::var("TON_CUDA_ARCH")
        && !arch.is_empty()
        && arch != "native"
    {
        compile.arg(format!("-arch={arch}"));
    }
    let status = compile
        .status()
        .unwrap_or_else(|e| panic!("failed to execute {}: {e}", nvcc.display()));
    if !status.success() {
        panic!("nvcc failed while compiling the CUDA backend");
    }

    // Let nvcc select the platform archiver so Windows uses a COFF/MSVC library
    // and Unix-like targets use their native archive format.
    let status = Command::new(&nvcc)
        .arg("--lib")
        .arg("-o")
        .arg(&archive)
        .arg(&obj)
        .status()
        .unwrap_or_else(|e| panic!("failed to package the CUDA object with nvcc: {e}"));
    if !status.success() {
        panic!("nvcc failed while packaging the CUDA library");
    }

    println!("cargo:rustc-link-search=native={}", out.display());
    println!("cargo:rustc-link-lib=static=ton_vanity_cuda");
    let runtime_dir = if is_windows {
        cuda_home.join("lib").join("x64")
    } else {
        cuda_home.join("lib64")
    };
    if !runtime_dir.exists() {
        panic!(
            "CUDA runtime library directory was not found: {}",
            runtime_dir.display()
        );
    }
    println!("cargo:rustc-link-search=native={}", runtime_dir.display());
    println!("cargo:rustc-link-lib=dylib=cudart");
    if !is_windows {
        println!("cargo:rustc-link-lib=dylib=stdc++");
    }
}
