//! The `djhost` goldens' fixture helper fails, naming the regen command, when a section's file is
//! missing (the other half of `djengine/tests/fixtures_required.rs`, which scans every crate's test
//! sources for a golden that would skip instead).

mod common_fixture;

fn panic_message(f: impl FnOnce() + std::panic::UnwindSafe) -> String {
    let err = std::panic::catch_unwind(f).unwrap_err();
    err.downcast_ref::<String>()
        .cloned()
        .or_else(|| err.downcast_ref::<&str>().map(|s| s.to_string()))
        .expect("a string panic")
}

#[test]
fn a_missing_swift_fixture_panics_and_names_the_regen_command() {
    let msg = panic_message(|| {
        common_fixture::swift_fixture("no-such-section");
    });
    assert!(msg.contains("no-such-section.json"), "{msg}");
    assert!(
        msg.contains("  tools/paritydump/regen.sh <swift app repo> HEAD no-such-section"),
        "{msg}"
    );
    assert!(!msg.contains("PARITYDUMP_CONFIG"), "{msg}");
}

/// The two sections `regen.sh` documents as release builds say so in the command.
#[test]
fn the_release_sections_name_their_build_config() {
    for section in ["journal", "automixdeck"] {
        assert_eq!(
            common_fixture::regen_command(section),
            format!(
                "PARITYDUMP_CONFIG=release tools/paritydump/regen.sh <swift app repo> HEAD {section}"
            )
        );
    }
    assert!(!common_fixture::regen_command("swap").contains("PARITYDUMP_CONFIG"));
}
