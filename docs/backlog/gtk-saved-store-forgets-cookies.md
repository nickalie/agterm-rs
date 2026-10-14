---
worth: yes
where: crates/agterm-gtk/src/html_overlay/registry.rs:229
added: 2026-10-15
---
# The GTK overlay's saved browser store keeps cookies in memory only

`persistent_store_failure` builds the saved store with `NetworkSession::new(data, cache)` and never gives its cookie
manager a file (`webkit_cookie_manager_set_persistent_storage`), and WebKit keeps a session's cookies in memory unless
told where to save them. So a `--persistent` page's login (a cookie) lasts only until agterm quits, while upstream's
`WKWebsiteDataStore(forIdentifier:)` keeps it across launches (`HtmlOverlayRegistry.swift`); local storage, which
WebKit writes to the data directory by itself, does survive. Found in Task 31: `agterm-wpe`'s engine test
`a_saved_profile_outlives_its_pages_and_its_session` read `cookie=;storage=v` from a new session on the same
directories until `agterm-wpe` set `<data>/cookies.sqlite` (SQLite) as the cookie file; WebKitGTK and WPE share that
code. Fix in `agterm-gtk`: the same call on the saved session's cookie manager, then a hand check that a login
survives a restart. The GPUI build gets it from `agterm-wpe`.
