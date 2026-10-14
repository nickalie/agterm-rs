# Spike: WPE WebKit offscreen frames into GPUI via dmabuf (real screen)

Plan: `docs/plans/20261014-gpui-frontend.md`, Task 2. Run 2026-10-14 on the user's real niri session (permission given
for this spike), isolated from the live agterm: AGTERM_*/ZMX_* stripped, every agterm instance private (own app id,
state dir, socket, zmx dir), nothing sent to the default socket.

Spike code and raw results: branch `spike/wpe-dmabuf` at commit `9e092fd` (`spikes/wpe-dmabuf/`,
removed from `gpui-frontend` in Task 4).

## Verdict

| criterion | dmabuf path | SHM fallback |
|---|---|---|
| 1080p60 without drops | **pass**: 0 dropped of 2034 (local VP9 1080p60), 0 of 2085 (YouTube AV1 1080p60), 59.6 fps delivered and presented | local VP9: **pass** (0 dropped, 59.4 fps); YouTube: **fail** (frames composited at 2.5 fps) |
| CPU ≈ the GTK build | **pass**: local video 10.3 % vs 19.3 % (GTK), page scroll 6.1 % vs 7.7 %, YouTube 39–45 % vs 34 % (codec and network differ between runs, see below) | **fail**: 80–115 % on the same video |
| zero-copy import into a wgpu texture (GPUI element, patched `gpui-pre`) | **pass** on Intel (3-plane CCS modifier) and NVIDIA (block-linear modifier); pixel readback matches the page | n/a (damage-rect upload or `RenderImage`) |
| input forwarding (pointer, wheel, keyboard) | **pass** through the forwarding path (key/click flip the page, wheel scroll at 60 fps); real hardware input not exercised by the automation | same |

Human check: while the spike ran, the user, watching the screen, reported «youtube нормально играется и
отображается» (YouTube plays and renders normally). Up to that message every YouTube run used the **dmabuf path
with WPE and GPUI both on the Intel iGPU (renderD128, niri's render GPU)** (`youtube-dmabuf-intel*` runs); the SHM
YouTube run came after it.

Overall: the HTML-overlay design holds (WPE WebKit offscreen + dmabuf into a wgpu texture), with conditions listed
under "Consequences for the plan". The SHM fallback is a degraded mode: fine for documents, not for video.

## What was built (`spikes/wpe-dmabuf/`)

- `csrc/wpe_shim.c` — WPE side, ~400 lines of C over the WPEPlatform API: a `WPEDisplayHeadless` on its own thread
  with its own `GMainContext` (pushed thread-default before any WebKit object exists; GPUI keeps the real main
  thread), a `WebKitWebView` loading the URL, ephemeral network session. Frames are taken by replacing
  `WPEViewHeadless`'s `render_buffer`/`buffers_changed` vfuncs at class level; the host acknowledges a buffer as
  rendered when GPUI paints it and releases it one frame later. Input arrives from the GPUI thread as idle sources on
  the WPE context (`wpe_event_pointer_*`, `wpe_event_scroll_new`, `wpe_event_keyboard_new` with the keycode from the
  display's `WPEKeymap`). SHM mode hooks the display's `get_drm_device`/`get_preferred_buffer_formats` to NULL and
  sets `WEBKIT_SKIA_ENABLE_CPU_RENDERING=1`.
- `src/wpe.rs` — FFI, frame hand-off (latest-frame slot + wake channel into a GPUI task), SHM damage-rect copy,
  GPUI→WPE key/modifier mapping.
- `src/main.rs` — GPUI app (`gpui-pre` 0.3.9 via `gpui-pre-platform`, no gpui-kit): a view with a `canvas` that paints
  the frame and GPUI mouse/wheel/key listeners that forward to WPE; scripted benches (`video`, `youtube`, `scroll`,
  `latency`) that evaluate JS in the page and write JSON results.
- `patches/gpui-pre.patch` — Linux payload for `PaintSurface` (`ExternalSurface::{DmaBuf, Pixels}`) and
  `Window::paint_external_surface`, modeled on macOS `paint_surface`.
- `patches/gpui-pre-wgpu.patch` — `external_surface.rs`: dmabuf import through wgpu-hal Vulkan (`VkImage` with
  `VK_IMAGE_TILING_DRM_FORMAT_MODIFIER_EXT` + explicit plane layouts, `VkImportMemoryFdInfoKHR` of a dup'ed fd,
  dedicated allocation, `texture_from_raw` → `create_texture_from_hal`), cached per buffer; SHM frames uploaded into a
  persistent texture by damaged rects (`queue.write_texture`); an RGBA fragment entry point on the existing surfaces
  pipeline; Vulkan device opened with `open_with_callback` to add `VK_EXT_image_drm_format_modifier` (wgpu enables
  `VK_KHR_external_memory_fd` and `VK_EXT_external_memory_dma_buf` itself, not the modifier extension).
- `scripts/` — `fetch-wpe.sh` (Arch `wpewebkit` package, signature checked against the pacman keyring, extracted, no
  root), `vendor.sh` (patched `gpui-pre`/`gpui-pre-wgpu` into `vendor/`), `run.sh` (bwrap overlay, see quirks),
  `probe.sh` (WPE side alone, no window), `bench.sh` + `measure.py` (CPU per process tree from `/proc`, NVIDIA per
  process from `nvidia-smi pmon`, Intel per DRM client from `fdinfo` engine time), `gtk-baseline.sh` (the GTK build's
  overlay as a private dev instance), `webkitgtk-check.py` (bare WebKitGTK 6 view), `gst-check.sh`.
- `bench/` — pages and a local server: `video.html` (1080p60 clip, MSE in 1 MB appends like a streaming player;
  `?progressive=1` for a plain `src`), `long.html` (600 sections, gradients, shadows; rAF auto-scroll), `latency.html`.
- `results/` — the JSON behind every number below.

Versions: WPE WebKit 2.54.1 (Arch `extra/wpewebkit` 2.54.1-2), WebKitGTK 2.54.1, GStreamer 1.28.8, `gpui-pre` 0.3.9
(pinned by gpui-kit 0.7.3), wgpu/wgpu-hal 29.0.4, ash 0.38.0, NVIDIA 615.78.08 (open), Mesa 26.2.4, niri 26.04,
Linux 7.2.9. Machine: Intel Iris Xe (Raptor Lake-P, renderD128, eDP) + RTX 4070 Laptop (renderD129, HDMI to the
3440x1440 monitor); **niri renders on the Intel GPU** (GPUI's compositor hint: 0x8086:0xa7a0 from 226:128).

Unit tests: `cargo test --release` (with `LD_LIBRARY_PATH` at the WPE prefix): 4 passed (arg parsing, interval
stats, XKB keyvals, WPE modifier bits). The spike's acceptance checks are the scripted runs below.

## Measurements

Window: niri tiles the spike window at 1714x1432 (the frame size in every run), video element 1920x1080 scaled into
it. CPU is % of one core over the measured window, summed over the process tree (UI process + WebKit children).
GPU: Intel engine busy % from the clients' `fdinfo` (`render`, `video`), niri's own render busy alongside; NVIDIA
per-process SM % where it applies. Whole-NVIDIA utilization sat at 20–30 % in every run including idle ones (other
users of the GPU) and is not attributed.

### 1080p60 video

Local clip: 60 s of YouTube format 303 (VP9 1920x1080 @ 59.94) of `LXb3EKWsInQ`, decoded by `vavp9dec` on the Intel
GPU (GStreamer's default here). 30 s windows.

| run | delivered fps | frames / dropped (page) | intervals > 25 ms | CPU % (web + UI) | Intel render / video % | niri render % |
|---|---|---|---|---|---|---|
| WPE dmabuf, all Intel | 59.6 | 2034 / 0 | 10 | **10.3** (5.4 + 4.9) | 17.2 / 7.2 | 18.7 |
| WPE dmabuf, all NVIDIA (`ZED_DEVICE_ID`, renderD129) | 59.1 | 2022 / 0 | 24 | 36.5 (28.4 + 8.0); SM 11 + 12 % | — / 7.7 | 17.5 |
| WPE SHM → damage-rect texture | 59.4 | 2025 / 0 | 7 | 80.1 (47.8 + 32.4) | 18.8 / 9.7 | 13.2 |
| WPE SHM → GPUI `RenderImage` | 58.2 | 2025 / 0 (44 GPUI-side gaps) | 44 | 114.6 (57.6 + 56.9) | 19.2 / 7.4 | 14.2 |
| GTK build overlay, MSE (same page) | — | **fails**: `Failed to push buffer` | — | 3.1 | — | 7.3 |
| GTK build overlay, progressive `src`, WebKit sandbox off | ≈59.7 (page) | 1906 / 0 | — | **19.3** (11.0 + 8.1) | 11.1 / 9.8 | 15.9 |

The GTK baseline had to use progressive download with the sandbox off: WebKitGTK fails this MSE stream (see quirks)
and stalls on progressive download with the sandbox on. WPE stalls on progressive download either way, so it uses
MSE. Same clip, decoder and size class; the delivery path differs (MSE vs progressive), which costs little CPU.

### YouTube 1080p60 (`https://www.youtube.com/watch?v=LXb3EKWsInQ`, EU consent page handled by script)

| run | stream | frames / dropped while playing | delivered fps | CPU % | Intel render / video % |
|---|---|---|---|---|---|
| WPE dmabuf, Intel (run 2) | AV1 10-bit (699) 1920x1080@60 + opus | 2085 / 0 | 54.2 (incl. buffering pauses) | 39.3 | 14.8 / 10.9 |
| WPE dmabuf, Intel (run 3) | VP9.2 (335) 1920x1080@60 + opus | 775 / 0 (network-limited, buffer health 0.01 s) | 49.3 | 45.0 | 17.5 / 3.9 |
| WPE dmabuf, Intel (run 1) | 1080p60 | 643 / 1 (the one drop at start-up) | 44.7 (8 s buffering stall) | 46.5 (console; its measure file was overwritten) | 13.3 / 3.3 |
| WPE SHM → texture | VP9.2 (335) 1920x1080@60 | page counts 1061 / 0, but **only 108 frames composited (2.5 fps)** | 2.5 | 43.9 | 0.3 / 5.5 |
| bare WebKitGTK 6 view (same engine as the GTK overlay) | VP9.2 (335) 1920x1080@60 | 0 dropped | — | 34.1 (29.1 web + 3.7 UI) | 16.8 / 8.2 |
| bare WebKitGTK 6 view (earlier run) | 1080p60 | 2478 / 0 by t = 41 s | — | 29.0 | 15.5 / 8.6 |
| GTK build overlay | does not autoplay (`media_playback_requires_user_gesture = true` in `html_overlay/page.rs`) | — | — | 31.2 (page JS only) | 1.6 / 0 |

One more dmabuf run (between runs 1 and 2) never started the video and starved JS evaluation with the web process
at 116 % CPU until the bench's timeout killed it; the bench now bounds every evaluation to 3 s. Dropped frames come
from `getVideoPlaybackQuality()`; codec and resolution from the player's stats for nerds. YouTube
picks the codec per run (AV1 or VP9.2) and the VPN link limited buffering; within those runs no frame was dropped by
rendering. Damage rects work: YouTube frames damaged 17–31 % of the view (the video area), video-only pages ~100 %.

### Long-page scroll (`long.html`, 1714x1432)

| run | page-driven (rAF, 8 px/frame) fps / > 25 ms | wheel-driven (120 Hz wheel through the forwarding path) fps / > 25 ms | CPU % |
|---|---|---|---|
| WPE dmabuf, Intel | 60.2 / 0 (page rAF 60.3) | 60.0 / 1 | 6.1 page-driven only; 23.0 incl. the wheel phase |
| WPE SHM → texture | 49.5 / 51 | 59.1 / 3 | 98.3 |
| WPE SHM → `RenderImage` | 59.4 / 4 | 59.2 / 3 | 79.1 |
| GTK build overlay | 60.3 / 0 (page rAF) | not measured (no input injection on the real session) | 7.7 |

### Input latency (input posted to WPE → first new frame from WPE; GPUI presents at the next vsync, ≤ 16.7 ms more)

Static page that flips its background on keydown/mousedown, 20 keys and 20 clicks each:

| run | key p50 / p95 / max ms | click p50 / p95 / max ms |
|---|---|---|
| WPE dmabuf, Intel | 13.9 / 18.5 / 24.4 | 9.0 / 17.2 / 23.7 |
| WPE dmabuf, NVIDIA | 12.2 / 20.2 / 21.2 | 14.6 / 19.3 / 20.5 |
| WPE SHM → texture | 41.2 / 46.1 / 50.9 | 41.1 / 52.5 / 58.1 |

Correctness: with `SPIKE_READBACK=1` the renderer copies the centre texel of every imported dmabuf texture back to
the CPU. Across 40 inputs it alternated exactly `#c03030` / `#303030` (XRGB, X = 0) on both Intel and NVIDIA, so
the import samples the right memory with the right modifier, offsets and strides.

## Driver and platform quirks met

1. **Cross-GPU dmabufs do not import.** WPE on NVIDIA hands `XR24` with NVIDIA block-linear modifier
   `0x0300000000e08014`; importing it into GPUI's device on the Intel GPU fails in `vkCreateImage`. The WPE render
   node must be the GPU GPUI's wgpu adapter uses (which GPUI picks from the compositor's GPU hint).
2. **Intel buffers have 3 memory planes** (CCS: main + aux + clear colour, one BO): the import has to pass every plane
   layout (non-disjoint image), not just plane 0.
3. **wgpu does not enable `VK_EXT_image_drm_format_modifier`**: the device has to be opened through wgpu-hal's
   `open_with_callback` (+ `create_device_from_hal`), i.e. a `gpui-pre-wgpu` patch, not only an element.
4. **WebKit's buffer set changes under the host.** The headless swapchain starts at 2 buffers, is destroyed and
   recreated on load and grows to 3; acknowledging a destroyed `WPEBuffer` trips `WPE_IS_BUFFER` criticals, and
   treating every `buffers_changed` as invalidation made WebKit allocate a new buffer per frame. The shim keeps the
   current set and drops stale acknowledgements; GPUI's import cache is invalidated only when a buffer leaves the set.
5. **No rendering fence**: the headless display uses implicit sync (`wpe_buffer_get_rendering_fence` is -1 on both
   drivers); content was correct anyway. Explicit sync (`use_explicit_sync`) was not explored.
6. **Acknowledging inside `render_buffer` re-enters WebKit**: `g_main_context_invoke` runs synchronously on the
   owner thread; acknowledgements go through idle sources instead. `wpe_event_pointer_button_new` asserts a zero press
   count on button-up.
7. **Media and the WebKit sandbox.** NVDEC (`nvcodec`) cannot initialise inside WebKit's bubblewrap sandbox
   (`CUDA_ERROR_NO_DEVICE`, no `/dev/nvidia-uvm`); the NVIDIA VA-API driver fails to initialise; so on this machine
   video decodes on the Intel GPU (`vavp9dec`), also when WebKit renders on NVIDIA (the cross-GPU hop is the 28 % web
   process CPU in the NVIDIA row). The user's GStreamer registry also cached `nvcodec` with no features (CUDA failed
   when it was built); a fresh registry lists the NVDEC decoders.
8. **Progressive media download stalls** ("Pausing stream for buffering", on-disk buffering to `/var/tmp` never
   completes): WPE with or without the sandbox, WebKitGTK only with it. MSE (how YouTube plays) works in WPE.
9. **WebKitGTK 2.54.1 fails this MSE stream** in the GTK build and in a bare WebKitGTK view: `vavp9dec`/`vah264dec`
   "DMABuf caps negotiated without the mandatory support of VideoMeta", and with VA decoders disabled
   `gst_pad_push() returned error`. WPE 2.54.1 plays the same page. YouTube itself plays in bare WebKitGTK.
10. **SHM mode cannot composite hardware-decoded 10-bit video**: CPU (Skia) rendering got the YouTube VP9.2 stream
    decoded (page counts frames, 0 dropped) but produced 2.5 frames/s.
11. **Distro WPE has its helper path compiled in** (`/usr/lib/wpe-webkit-2.0/WPEWebProcess`, no `WEBKIT_EXEC_PATH`
    in release builds): the spike overlays the extracted package on `/usr/lib` with `bwrap --ro-overlay`; a packaged
    agterm would depend on the distro's `wpewebkit` instead.
12. **GPUI exits with SIGSEGV** after the window closes: `eglTerminate` from wgpu's GLES instance in a TLS destructor
    after the Wayland connection is gone (stack: `__call_tls_dtors` → `drop<wgpu_core::global::Global>` →
    `wgpu_hal::gles::egl::Inner` → `wl_proxy_marshal_array_flags`). Unrelated to the dmabuf code, reproducible on
    every exit; an upstream gpui/wgpu candidate.
13. niri tiles new windows (1714x1432 here), so sizes cannot be forced; GPUI resizes WPE to the element bounds.

## Consequences for the plan (input to Task 4, and Tasks 29–32)

- Keep WPE WebKit offscreen + dmabuf as the HTML overlay design; the frame path, input path and the GPUI element
  shape are proven. Plan the SHM path as a fallback for documents only (no video), or drop "SHM with damage rects"
  as a performance path.
- Task 29 (`gpui-pre` patch) needs three parts, not one: the scene primitive + window API, the renderer draw, and the
  device creation (`open_with_callback` for `VK_EXT_image_drm_format_modifier`). Multi-plane non-disjoint import is
  required on Intel. Upstreaming to Zed would be a `PaintSurface` Linux payload, as here.
- Task 31 (`agterm-wpe`): the WPE device must follow GPUI's adapter (pass the render node of the wgpu adapter's DRM
  device, e.g. via `VK_EXT_physical_device_drm`), and the display's preferred formats should be the modifiers the
  Vulkan device can import. Implement a proper `WPEDisplay`/`WPEView` subclass instead of patching the headless
  classes' vfuncs; keep the buffer-set tracking and the deferred acknowledgements.
- Video on hybrid machines decodes on the Intel GPU regardless of where WebKit renders; rendering WPE on the
  compositor's GPU (as GPUI already chooses) is also the cheapest configuration measured (10 % CPU vs 36 % on NVIDIA).
- The GTK build's overlay requires a user gesture for media; the WPE port keeps that setting, and the baseline above
  is the engine, not the overlay UI.

## Deviations from the plan

- The spike code and this report are committed on the shared `gpui-frontend` branch (spike files under
  `spikes/wpe-dmabuf/`, outside the cargo workspace), not on a `spike/wpe-dmabuf` branch: this run could only write
  to that worktree, and the coordinator directed the commit there.
- WPE WebKit comes from Arch's binary package in a private prefix (no root, no source build); the patched `gpui-pre`
  crates are kept as patches applied to the crates.io sources by `scripts/vendor.sh`.
- The GTK baseline for the local clip uses progressive download with the WebKit sandbox off, and the YouTube baseline
  uses a bare WebKitGTK 6 view, because the GTK overlay cannot play either otherwise (quirks 8–9, gesture setting).
- Pointer-move forwarding and real hardware input into the GPUI window were not exercised automatically (no input
  injection on the user's session); keyboard, button and wheel forwarding were driven through the same forwarding
  functions the GPUI listeners call.

## Reproducing

```
cd spikes/wpe-dmabuf
scripts/fetch-wpe.sh && scripts/vendor.sh
CARGO_BUILD_JOBS=6 cargo build --release
python3 -I bench/server.py 18431 .cache/media results/raw/reports.jsonl &   # media: yt-dlp -f 303 / 299, 60 s sections
scripts/bench.sh video-dmabuf-intel 30 --mode dmabuf --device /dev/dri/renderD128 \
  --url http://127.0.0.1:18431/video.html --bench video
scripts/gtk-baseline.sh 'http://127.0.0.1:18431/long.html?auto=1' 8 results/raw/scroll-gtk.measure.json
```
