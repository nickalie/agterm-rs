// Generated once by gir (`split_build_rs`, see Gir.toml) and kept by hand since: gir regenerates only
// build_version.rs, which this script does not use (the crate binds no version-gated API).
//
// Probes wpe-webkit-2.0 through pkg-config (scripts/pkg-config.sh: the system's WPE WebKit, else the vendored package
// of scripts/fetch-wpe.sh) and, when there is none, says where one comes from. A vendored WPE lies outside the loader's search path, so this crate's own tests get an
// rpath to every library directory the probe reports (pkg-config leaves the system ones out); its dependents link as
// usual and run WPE from where their launcher puts it.

fn main() {
    if std::env::var("DOCS_RS").is_ok() {
        // prevent linking libraries to avoid documentation failure
        return;
    }

    match system_deps::Config::new().probe() {
        Ok(deps) => {
            for path in deps.all_link_paths() {
                println!("cargo::rustc-link-arg-tests=-Wl,-rpath,{}", path.display());
            }
        }
        Err(s) => {
            println!(
                "cargo::error={}: this crate needs WPE WebKit 2.54 or newer: install the distribution's wpewebkit or \
                 run scripts/fetch-wpe.sh, or build without the WPE crates (\"${{WORKSPACE_WITHOUT_WPE[@]}}\" of \
                 scripts/lib/native.sh in place of --workspace); see README > WPE WebKit",
                s.to_string().replace('\n', " ")
            );
        }
    }
}
