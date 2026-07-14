use std::{env, fs, path::PathBuf};

fn embed_windows_resources() {
    if env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }

    println!("cargo:rerun-if-changed=assets/windows.rc");
    println!("cargo:rerun-if-changed=assets/tray-on.ico");
    let version_macros = [
        format!(
            "VERSION_MAJOR={}",
            env::var("CARGO_PKG_VERSION_MAJOR").expect("Cargo package major version")
        ),
        format!(
            "VERSION_MINOR={}",
            env::var("CARGO_PKG_VERSION_MINOR").expect("Cargo package minor version")
        ),
        format!(
            "VERSION_PATCH={}",
            env::var("CARGO_PKG_VERSION_PATCH").expect("Cargo package patch version")
        ),
    ];
    embed_resource::compile_for("assets/windows.rc", ["quest-vd-wired"], &version_macros)
        .manifest_required()
        .expect("embedding the Windows executable resources");
}

fn main() {
    embed_windows_resources();
    println!("cargo:rerun-if-env-changed=GNIREHTET_VD_APK");
    println!("cargo:rerun-if-env-changed=CARGO_CFG_FUZZING");
    let manifest = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").unwrap());
    let profile = env::var("PROFILE").unwrap_or_else(|_| "debug".into());
    let fuzzing = env::var_os("CARGO_CFG_FUZZING").is_some();
    let configured = env::var_os("GNIREHTET_VD_APK").map(PathBuf::from);
    let release =
        manifest.join("../../../android-v4/app/build/outputs/apk/release/app-release.apk");
    let debug = manifest.join("../../../android-v4/app/build/outputs/apk/debug/app-debug.apk");
    println!("cargo:rerun-if-changed={}", release.display());
    println!("cargo:rerun-if-changed={}", debug.display());
    let source = configured.or_else(|| {
        if profile == "release" {
            release.is_file().then_some(release.clone())
        } else {
            debug
                .is_file()
                .then_some(debug.clone())
                .or_else(|| release.is_file().then_some(release.clone()))
        }
    });
    let generated = PathBuf::from(env::var_os("OUT_DIR").unwrap()).join("embedded_apk.rs");
    if let Some(source) = source.filter(|path| path.is_file()) {
        let embedded = generated.with_file_name("gnirehtet-v4.apk");
        fs::copy(&source, &embedded).expect("copying embedded Android v4 APK");
        fs::write(
            generated,
            format!(
                "pub const EMBEDDED_APK: Option<&[u8]> = Some(include_bytes!(r#\"{}\"#));\n",
                embedded.display()
            ),
        )
        .expect("writing embedded APK source");
    } else {
        if profile == "release" && !fuzzing {
            panic!(
                "release build requires GNIREHTET_VD_APK or android-v4/app/build/outputs/apk/release/app-release.apk"
            );
        }
        println!(
            "cargo:warning=Android v4 APK is not embedded; `start` will fail explicitly (set GNIREHTET_VD_APK)"
        );
        fs::write(generated, "pub const EMBEDDED_APK: Option<&[u8]> = None;\n")
            .expect("writing missing embedded APK marker");
    }
}
