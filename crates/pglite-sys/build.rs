use sha2::Digest;
use std::env;
use std::path::{Path, PathBuf};
use std::process::Command;

const ENGINE_TAG: &str = "engine-06c837c6a303-p3";
const RELEASE_BASE: &str = "https://github.com/Midwess/pglite-rs/releases/download";

fn main() {
    println!("cargo:rerun-if-env-changed=PGLITE_LIB_DIR");
    println!("cargo:rerun-if-env-changed=PGLITE_NO_DOWNLOAD");

    if env::var("DOCS_RS").is_ok() {
        return;
    }

    let lib_dir = resolve_lib_dir();
    println!("cargo:lib_dir={}", lib_dir.display());
    // Published artifact crates ship libpglite.a.zst (the engine is too large
    // for crates.io's 10 MiB limit otherwise); decompress it into OUT_DIR and
    // link from there. Local/dev trees and the download fallback carry a plain
    // libpglite.a, in which case the lib dir is used directly.
    let link_dir = prepare_static_lib(&lib_dir);
    println!("cargo:rustc-link-search=native={}", link_dir.display());
    println!("cargo:rustc-link-lib=static:+whole-archive=pglite");
    println!("cargo:rustc-link-lib=z");
    match env::var("CARGO_CFG_TARGET_OS").as_deref() {
        Ok("macos") => {
            println!("cargo:rustc-link-arg=-Wl,-export_dynamic");
            if env::var("CARGO_FEATURE_ICU").is_ok() {
                println!("cargo:rustc-link-lib=c++");
            }
        }
        Ok("windows") => {
            println!("cargo:rustc-link-lib=ws2_32");
            println!("cargo:rustc-link-lib=secur32");
        }
        _ => {
            println!("cargo:rustc-link-arg=-Wl,--export-dynamic");
            if env::var("CARGO_FEATURE_ICU").is_ok() {
                println!("cargo:rustc-link-lib=stdc++");
            }
        }
    }
}

fn prepare_static_lib(lib_dir: &Path) -> PathBuf {
    if env::var("PGLITE_NO_DOWNLOAD").as_deref() == Ok("1") {
        let target = env::var("TARGET").expect("Cargo supplies TARGET");
        let manifest = lib_dir.join("pglite-artifacts.manifest");
        println!("cargo:rerun-if-changed={}", manifest.display());
        let contents = std::fs::read_to_string(&manifest).unwrap_or_else(|error| {
            panic!(
                "cannot read local native artifact manifest {}: {error}",
                manifest.display()
            )
        });
        assert!(
            contents
                .lines()
                .any(|line| line == format!("target={target}")),
            "local native artifacts do not declare Cargo target {target}"
        );
        let archive = lib_dir.join("libpglite.a");
        let checksum = PathBuf::from(format!("{}.sha256", archive.display()));
        println!("cargo:rerun-if-changed={}", archive.display());
        println!("cargo:rerun-if-changed={}", checksum.display());
        let expected = std::fs::read_to_string(&checksum).unwrap_or_else(|error| {
            panic!(
                "cannot read native artifact checksum {}: {error}",
                checksum.display()
            )
        });
        let bytes = std::fs::read(&archive).unwrap_or_else(|error| {
            panic!("cannot read native engine {}: {error}", archive.display())
        });
        let actual = format!("{:x}", sha2::Sha256::digest(bytes));
        assert_eq!(
            expected.trim(),
            actual,
            "checksum mismatch for local native engine {}",
            archive.display()
        );
    }
    let zst = lib_dir.join("libpglite.a.zst");
    if zst.exists() {
        let out_dir = PathBuf::from(env::var("OUT_DIR").unwrap());
        zstd_decompress(&zst, &out_dir.join("libpglite.a"));
        return out_dir;
    }
    lib_dir.to_path_buf()
}

fn zstd_decompress(src: &Path, dst: &std::path::Path) {
    use std::io::Read;
    let f = std::fs::File::open(src).unwrap_or_else(|e| panic!("open {}: {e}", src.display()));
    let mut decoder = ruzstd::decoding::StreamingDecoder::new(f)
        .unwrap_or_else(|e| panic!("zstd init for {}: {e}", src.display()));
    let mut buf = Vec::new();
    decoder
        .read_to_end(&mut buf)
        .unwrap_or_else(|e| panic!("zstd decode {}: {e}", src.display()));
    std::fs::write(dst, buf).unwrap_or_else(|e| panic!("write {}: {e}", dst.display()));
}

fn variant_subdir() -> &'static str {
    if env::var("CARGO_FEATURE_ICU").is_ok() {
        "icu"
    } else {
        ""
    }
}

fn asset_stem() -> &'static str {
    if env::var("CARGO_FEATURE_ICU").is_ok() {
        "pglite-icu"
    } else {
        "pglite"
    }
}

fn resolve_lib_dir() -> PathBuf {
    if let Ok(dir) = env::var("PGLITE_LIB_DIR") {
        return PathBuf::from(dir);
    }

    if let Some(dir) = artifact_lib_dir() {
        return dir;
    }

    let local = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap())
        .join("../../native/out")
        .join(variant_subdir());
    if local.join("libpglite.a").exists() {
        return local;
    }

    download_prebuilt()
}

fn artifact_lib_dir() -> Option<PathBuf> {
    if env::var("CARGO_FEATURE_ICU").is_ok() {
        return None;
    }
    // Cargo sets exactly one DEP_PGLITE_LIB_<links>_ROOT, for the artifact
    // crate selected by this target's cfg. Scan for it rather than rebuilding
    // the name from TARGET, since the windows-gnu lib crate also serves the
    // windows-gnullvm target (different TARGET, same links key).
    for (key, val) in env::vars() {
        if key.starts_with("DEP_PGLITE_LIB_") && key.ends_with("_ROOT") {
            let dir = PathBuf::from(val).join("lib");
            if dir.join("libpglite.a").exists() {
                return Some(dir);
            }
        }
    }
    None
}

fn download_prebuilt() -> PathBuf {
    assert!(
        env::var("PGLITE_NO_DOWNLOAD").as_deref() != Ok("1"),
        "PGLITE_NO_DOWNLOAD=1 and no local native engine was found; build it with native/build-libpglite.sh or set PGLITE_LIB_DIR"
    );
    let target = env::var("TARGET").unwrap();
    let cache = env::var("HOME")
        .or_else(|_| env::var("USERPROFILE"))
        .map(PathBuf::from)
        .unwrap_or_else(|_| env::temp_dir())
        .join(".cache/pglite-rs")
        .join(ENGINE_TAG)
        .join(&target)
        .join(variant_subdir());

    if cache.join("libpglite.a").exists() {
        return cache;
    }
    std::fs::create_dir_all(&cache).expect("cannot create cache dir");

    let asset = format!("{}-{target}.tar.gz", asset_stem());
    let url = format!("{RELEASE_BASE}/{ENGINE_TAG}/{asset}");
    let tarball = cache.join(&asset);

    fetch(&url, &tarball);
    fetch(
        &format!("{url}.sha256"),
        &cache.join(format!("{asset}.sha256")),
    );

    let expected = std::fs::read_to_string(cache.join(format!("{asset}.sha256")))
        .expect("cannot read checksum file");
    let actual = sha256_of(&tarball);
    assert_eq!(
        expected.trim(),
        actual.trim(),
        "checksum mismatch for {asset}; delete {} and retry",
        cache.display()
    );

    let status = Command::new("tar")
        .args([
            "xzf",
            tarball.to_str().unwrap(),
            "-C",
            cache.to_str().unwrap(),
        ])
        .status()
        .expect("failed to run tar");
    assert!(status.success(), "failed to extract {asset}");

    cache
}

fn fetch(url: &str, dest: &std::path::Path) {
    let status = Command::new("curl")
        .args(["-fsSL", "--retry", "3", "-o", dest.to_str().unwrap(), url])
        .status()
        .expect("failed to run curl; install curl or set PGLITE_LIB_DIR");
    assert!(
        status.success(),
        "failed to download {url}; build the engine locally with native/build-libpglite.sh or set PGLITE_LIB_DIR"
    );
}

fn sha256_of(path: &std::path::Path) -> String {
    let output = Command::new("shasum")
        .args(["-a", "256", path.to_str().unwrap()])
        .output()
        .or_else(|_| Command::new("sha256sum").arg(path).output())
        .expect("need shasum or sha256sum to verify download");
    assert!(output.status.success(), "checksum tool failed");
    String::from_utf8_lossy(&output.stdout)
        .split_whitespace()
        .next()
        .unwrap()
        .to_string()
}
