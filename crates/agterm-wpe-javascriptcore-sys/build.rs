// Generated once by gir (`split_build_rs`, see Gir.toml) and kept by hand since: gir regenerates only
// build_version.rs, which this script does not use (the crate binds no version-gated API).
//
// Probes WPE WebKit through pkg-config (scripts/pkg-config.sh: the system's, else the vendored package of
// scripts/fetch-wpe.sh) and, when there is none, says where one comes from.

fn main() {
    if std::env::var("DOCS_RS").is_ok() {
        // prevent linking libraries to avoid documentation failure
        return;
    }

    if let Err(s) = system_deps::Config::new().probe() {
        println!(
            "cargo::error={}: this crate needs WPE WebKit 2.54 or newer: install the distribution's wpewebkit or run \
             scripts/fetch-wpe.sh, or build without the WPE crates (\"${{WORKSPACE_WITHOUT_WPE[@]}}\" of \
             scripts/lib/native.sh in place of --workspace); see README > WPE WebKit",
            s.to_string().replace('\n', " ")
        );
    }
}
