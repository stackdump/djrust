//! Build-time settings (README.md, "Build-time settings"): the namespace of on-disk format IDs and
//! of the app-support folder, the default ssh target for installing a PD705 kit, and the CDN a
//! published session's public URL points at. An app with
//! existing files sets its own in `.cargo/config.toml` `[env]`.
fn main() {
    let word = |c: char| c.is_ascii_alphanumeric() || "-_.@".contains(c);
    let url = |c: char| word(c) || ":/~%".contains(c);
    let settings: [(&str, &str, &dyn Fn(&str) -> bool, &str); 3] = [
        ("DJRUST_FORMAT_NS", "djrust", &|v| v.chars().all(word), "letters, digits and - _ . @"),
        ("DJRUST_PD705_HOST", "pi@raspberrypi.local", &|v| v.chars().all(word), "letters, digits and - _ . @"),
        (
            "DJRUST_PUBLISH_BASE",
            "https://cdn.bitwrap.io/ipfs/",
            &|v| (v.starts_with("https://") || v.starts_with("http://")) && v.ends_with('/') && v.chars().all(url),
            "an http(s) URL ending in /",
        ),
    ];
    for (var, default, valid, what) in settings {
        println!("cargo:rerun-if-env-changed={var}");
        let v = std::env::var(var).unwrap_or_else(|_| default.into());
        assert!(!v.is_empty() && valid(&v), "{var}={v:?}: use {what}");
        println!("cargo:rustc-env={var}={v}");
    }
}
