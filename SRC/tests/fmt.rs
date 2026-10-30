// Part of the APK.audio project — http://APK.audio — made by Anthony Kuzub
// MIT Licence. Free, for everyone, for ever. Full text in LICENSE at the root.
//
// THE CRATE STAYS RUSTFMT-CLEAN. Nothing in CI or check.sh runs rustfmt
// (the root ci.yml verifies nothing on purpose; enforcement is the operator's
// `cargo test`), so the check lives here, in the suite that does get run.
// A missing rustfmt is a failure, not a skip: a check that silently passes
// when it cannot look is the same as no check.

use std::process::Command;

#[test]
fn crate_is_rustfmt_clean() {
    let manifest = concat!(env!("CARGO_MANIFEST_DIR"), "/Cargo.toml");
    let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".into());
    let out = Command::new(cargo)
        .args([
            "fmt",
            "--check",
            "-p",
            env!("CARGO_PKG_NAME"),
            "--manifest-path",
            manifest,
        ])
        .output()
        .expect("cargo fmt could not be started (install rustfmt: rustup component add rustfmt)");
    assert!(
        out.status.success(),
        "cargo fmt --check failed; run `cargo fmt -p {}` in {}\n{}{}",
        env!("CARGO_PKG_NAME"),
        env!("CARGO_MANIFEST_DIR"),
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
    );
}
