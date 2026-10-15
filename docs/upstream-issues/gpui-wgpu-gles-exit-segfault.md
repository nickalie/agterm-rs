# Draft: GPUI on Linux segfaults at exit in wgpu's GLES teardown after the Wayland connection is gone

Drafted for zed-industries/zed (GPUI), with a pointer for gfx-rs/wgpu; **not posted** (plan
`docs/plans/20261014-gpui-frontend.md`, Task 12: this run never posts to external sites). Post it by hand, after
re-checking against current Zed `main`.

## Issue text

**Title:** GPUI (Linux, Wayland): SIGSEGV on every exit, `eglTerminate` from wgpu's GLES instance in a TLS destructor
after the Wayland connection is closed

**Environment:** GPUI as published in `gpui-pre` 0.3.9 (snapshot of zed@93d7456, wgpu / wgpu-hal 29.0.4), Linux,
niri (Wayland), NVIDIA proprietary driver with the Intel iGPU as the compositor's GPU; the window renders through
wgpu's Vulkan backend.

**What happens:** the process ends with SIGSEGV after the last window closed and the application quit, on every exit.
The stack:

```
__call_tls_dtors
  → drop_in_place<wgpu_core::global::Global>
    → wgpu_hal::gles::egl::Inner (drop: eglTerminate)
      → wl_proxy_marshal_array_flags        ← the Wayland connection is already closed
```

**Why:** `gpui_wgpu::WgpuContext::instance` (`crates/gpui_wgpu/src/wgpu_context.rs`, the `instance` fn) creates the
instance with `wgpu::Backends::VULKAN | wgpu::Backends::GL`, so wgpu-core initialises a GLES/EGL instance on the
Wayland display even when the adapter GPUI selects is a Vulkan one. That instance is dropped from a thread-local
destructor at process exit, after GPUI's Wayland client has disconnected, and its `eglTerminate` still talks to the
dead `wl_display`.

**Expected:** a clean exit (status 0).

**Possible fixes (either side):**

- GPUI: create the GL backend only when no Vulkan adapter works (a second instance), or drop the wgpu instance before
  the Wayland connection closes (explicit teardown order in the platform's shutdown instead of a TLS destructor).
- wgpu: do not marshal on a disconnected display in `egl::Inner::drop` (or make the EGL display's lifetime not
  outlive the native display it was created on).

**Workaround in our app:** after the orderly shutdown and `Application::run` returning, flush stdio and `_exit(status)`
so no TLS destructor runs (`std::process::exit` does not help: glibc's `exit` runs `__call_tls_dtors` first).

## Notes for whoever posts it

- Source of the stack: `docs/spikes/20261014-wpe-dmabuf.md`, quirk 12 (Task 2 spike, real niri session, NVIDIA).
- Not reproduced on Mesa in Task 12: a windowed agterm-gpui on a private headless weston quit with status 0 with the
  plain `std::process::exit`, both when wgpu picked Mesa's Intel GL adapter and when it picked lavapipe (Vulkan).
  The crash may need NVIDIA's EGL; reproduce on such a machine before posting.
- agterm-gpui keeps the `_exit` workaround (`crates/agterm-gpui/src/main.rs`, `exit_now`) until a fixed GPUI is
  vendored; then drop it and check the exit status on NVIDIA.
