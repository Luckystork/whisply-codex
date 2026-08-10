fn main() {
    println!("cargo:rerun-if-env-changed=CARGO_FEATURE_TEST_SUPPORT");
    println!("cargo:rerun-if-env-changed=PROFILE");

    if std::env::var_os("CARGO_FEATURE_TEST_SUPPORT").is_some()
        && std::env::var("PROFILE").as_deref() == Ok("release")
    {
        panic!("codex-whisply test-support must never be enabled in release builds");
    }
}
