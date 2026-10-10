# zgui patch to wgpu-hal 29.0.4 (Metal backend only)

Vendored from crates.io `wgpu-hal` 29.0.4 (MIT OR Apache-2.0, license files
unchanged) and applied through `[patch.crates-io]` in the workspace
`Cargo.toml`. Only `src/metal/{command,mod,device,adapter}.rs` differ, plus
the new `src/metal/buffer_writes.rs`; search for "zgui patch".

## Why

Upstream creates a real `MTLCommandBuffer` for every hal encoder that
wgpu-core opens, including the internal "transit" encoders around each pass
and submission that usually record nothing, and commits a separate command
buffer just to present. A zgui frame with two render passes committed about
seven command buffers. On Apple silicon each one costs a commit, an IOKit
submission, completion callbacks on GCD worker threads and Core Animation
frame-pacing IPC (`FPCAMetalLayerState`), which showed up as kernel time: 2.6%
system CPU for zgui vs 1.4% for GPUI on the same animation
(`comparisons/*/animate`, `zgui-desktop/examples/animation_workload`).

## What

1. **Lazy command buffers.** `begin_encoding` records the label; the Metal
   command buffer is created on first use. Encoders that record nothing end
   with no buffer, and `submit` skips them. The fence signal attaches to the
   last real buffer (or, as upstream, to an internal one when a submission is
   entirely empty), so completion ordering is unchanged.
2. **Present without an extra command buffer.** `presentDrawable:` presents
   from the command buffer's scheduled handler. `submit` now registers that
   handler on its last buffer before commit, and `present` hands the drawable
   to it: whichever happens second presents, without blocking. Queue order
   makes the last submission's scheduling cover all earlier rendering. When
   there is no recorded submission, or for transactional presents, the
   upstream path is used.

3. **Buffer fills and copies without blit encoders.** `clear_buffer` and
   `copy_buffer_to_buffer` run as vertex-only render passes (rasterization
   off, one vertex per 32-bit word; see `buffer_writes.rs`), falling back to
   blits for unaligned ranges, same-buffer copies or pending timer queries.
   The first blit encoder a process uses makes the Apple silicon driver hold
   about 110 MB for roughly eight seconds and some of it for good, and
   wgpu-core issues two buffer blits while creating every device. A small
   zgui window measured 297 MB of graphics memory at launch and 65 MB at rest
   before, 185 MB and 41 MB after. Each pass renders to a 1x1 private
   texture it neither loads nor stores: two attachment-less passes in one
   command buffer (wgpu-core's fill and copy at device creation) made the
   driver keep a second set of six 512 KB regions for the life of the
   process, 3 MB of footprint. Tested by
   `crates/zgui-gpu/tests/buffer_writes.rs`.

4. **Exact waits.** `Device::wait` without a timeout blocks on the command
   buffer (`waitUntilCompleted`) instead of polling its status every
   millisecond, so a renderer waiting for its frame before presenting it
   (zgui's own surfaces, `GpuRenderer::present_layer`) presents as the GPU
   finishes rather than up to a millisecond later.

5. **No per-submission extras nothing reads.** Fences have no
   `MTLSharedEvent` (only `Fence::raw_shared_event` interop read it, and
   signalling it is kernel work in every submission), and the scheduled
   handler that presents drawables is registered only on submissions that
   use a surface texture.

Result: two command buffers per frame (one per render pass), 1.2 ms less
kernel time per 100 frames, and zgui below GPUI in CPU per frame.
