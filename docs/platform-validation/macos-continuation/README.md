# macOS continuation: runtime evidence and remaining gates

**Qualification is partial.** This packet records real Apple M5 Max execution on
macOS 26.4, including Metal/CoreVideo, native windows, AppKit dialogs/menus,
clipboard exchange, accessibility and dead-key input. It also preserves failures
and their fixes. It does not establish that every native acceptance case is
complete or that zgui has universally minimal CPU/memory use.

The [capability matrix](../../gpui-parity.md) defines the finite acceptance scope.
[The handoff](../../MACOS-HANDOFF.md) gives continuation commands and exact pending
native checks. [Archive hashes](archives.json) identify retained compressed
packets; [the earlier evidence inventory](EVIDENCE.md) records their chronological
scope and source limitations. No earlier failed artifact was replaced by a pass.

## Automated checks and source boundaries

| Evidence | Recorded result | What it establishes / limits |
| --- | --- | --- |
| [Initial Actions inspection](ci-initial/README.md), run `35795413672`, commit `e51589408079f831c8b9ee433388e2edf46432a7` | Failed | Mac tests used Control where Command is required; Linux linker SIGBUS has no established root cause; artifact quota prevented GPU artifact uploads. Workflow mitigations need a new remote run. |
| `zgui-macos-validation-20260922-01.tar.gz` | Failed GPU bundle | Retains initial font-dependent failures. Its empty manual list was a harness initialization limitation, not evidence that native checks passed. |
| `zgui-metal-fixes.tar.gz` | Focused fixes verified | Bundled font assumptions and realistic platform command modifiers corrected; real Metal/CoreVideo checks ran. Does not establish GUI interaction by itself. |
| `zgui-macos-validation-20260922-02.tar.gz` | Automated subset passed | Complete GPU suite, AX projection tests, four-frame native surface window, rich-text window, component idle/active host, close-policy/multiwindow lifetime and window resize checks. All 18 manual bundle entries remain pending within that run. Its source predates later native fixes. |
| `zgui-macos-quality-01.tar.gz` / `-02.tar.gz` | Full local checks passed at their recorded sources | Workspace all-target tests, doctests, formatting and strict Clippy. Quality-02 includes reviewed menu and recursive owner-sheet dismissal fixes; these runs predate later AppleEvent/IME repairs. |
| `zgui-macos-quality-03.tar.gz` | Full local checks and release workload build passed | Baseline after AppleEvent ownership and borderless maximize-query fixes. Raw logs, source manifest/diff and executable hash identify that state. |
| `zgui-macos-quality-04.tar.gz` | Focused IME tests, formatting, strict all-target Clippy and release build passed | Follow-up after native input-context reset changes. The immediately preceding full-suite baseline is Quality-03; focused tests are not described as a new full-suite run. |
| [Bundle 03](zgui-macos-validation-20260922-03.tar.gz) | Automated subset passed | Records the complete bundle at the production IME deactivation fix, including corrected source archive coverage. Source tar SHA256 `5e563c857caca17ec181247b6b19deffea987ad8ac69847dabee6b0cd0ad5478`. GUI cases remain separately qualified below. |
| [Final bundle 04](zgui-macos-validation-20260922-04.tar.gz) | Automated subset passed at `8e577cbd2b2bd3065f80a72cea7081fb49a2a5b4` | Repeats complete Metal/CoreVideo and native probes after equivalent RGBA-loop lint repair and observer fixtures. Source tar SHA256 `afbe9012533cee4309bcb5ac5c8c4888fc9cd9d1e7ec002ecb97b519e5dea1ca`. This is the latest archived automated bundle; separate UI limits remain. |
| [Final native repair/experiment logs](zgui-metal-fixes-final.tar.gz) | Completed chronological packet | Includes final builds/checks and discarded synchronous-drag patch; the earlier metal-fixes archive remains immutable. Experiments are not all delivered fixes. |
| [Fixture-order follow-up](zgui-macos-fixture-order-followup.tar.gz) | Formatting/build/strict Clippy passed at `7df5c86` | Added multi-file/window-state controls were moved after existing controls to preserve Linux coordinate-based fixture scripts. Includes commit patch and later logs separately; production code and earlier resource identities are unchanged. This is not a new native interaction run. |
| [Linux XIM follow-up](zgui-linux-native-ime-followup.tar.gz) | Default synchronous mode failed; original-stack asynchronous checks passed | Preserves default failure, raw host/XIM traces and rejected private winit candidate. Original binaries pass eight-stage Pinyin/candidate/focus checks, model/read-only cancellation and ordinary modifier/text/delete recovery with owned asynchronous IBus mode. Default IBus 1.5.29 compatibility remains limited. |
| [Physical same-scale display movement](zgui-macos-display-01.tar.gz) | Native monitor/bounds readback passed; normal exit 0 | Two attached 5120×2880 displays at scale 2.0; monitor 0→1→0 and exact bounds each settled over ten 100 ms snapshots. New opt-in example is included verbatim, production source `cca2ead` unchanged. Mixed-scale/hotplug/pixel restoration are not inferred. |
| [Current-source native GIF](zgui-macos-gif-02.tar.gz), [earlier wrapper experiment](zgui-macos-gif-01.tar.gz) | File decode, presentation, pause/resume and normal close observed | Current `cca2ead` binary in a distinctly named wrapper held identical blue preview pixels for 46.082 seconds, then resumed red/green and exited 0. Three opaque full-canvas frames with Background disposal are not complex-disposal or cadence proof. Earlier launch/connection ambiguity remains recorded. |
| [Final UI packet](zgui-macos-ui-20260922-01.tar.gz) | Observations qualified in the 18-case table | Logs, test data, chronology and old/final capture-time identities. All `.app` trees/executable binaries excluded; native-platform wrapper was the discarded experiment. |
| [Resource probe 01](zgui-macos-resources-01.tar.gz) | All six functional/sampling trials passed | Three release idle/active pairs, raw samples, hashes, source archive and CPU-unit self-check; measured values below. This is not a matched comparison. |
| [Final resource probe 02](zgui-macos-resources-02.tar.gz) | All six trials passed on rebuilt `8e577cb` production source | Fresh measurements after the equivalent RGBA-loop change; release build log, all samples and exact source/binary identities retained. Later fixture-only edits are separate from these production measurements. |
| [Pushed CI at `475fe92`](https://github.com/zeronsh/zgui/actions/runs/35807926924), [retained packet](ci-reviewed.tar.gz) | Failed | Exposed a Rust 1.98 Clippy diagnostic, fixed-delay native lifecycle race and missing Linux `xmodmap`. Later corrections and runs are separate evidence below; local results do not imply remote success. |
| [Linux ARM compositor readiness probe](zgui-linux-sway-readiness.tar.gz) | Five fresh native compositor starts passed | Official Ubuntu 24.04 ARM64 container, bounded to two CPUs/2 GiB. Actual Xvfb/Openbox/Sway startup and active X11-1 output verified. No Rust application was built or exercised. |
| [Readiness-corrected CI at `a89d8d0`](https://github.com/zeronsh/zgui/actions/runs/35810106069), [retained packet](ci-reviewed-06.tar.gz) | Three jobs passed; Linux stopped at a later trace-parser check | Actual nested menu/dialog/file-drop and variable/measured-list paths now passed. Wayland input setup then rejected `@` object IDs while expecting `#`, despite correct configure/attach events. Downstream input actions were not reached. |
| Follow-up CI at `7255f67` | [Failed run 09](https://github.com/zeronsh/zgui/actions/runs/35812238385), [packet](ci-reviewed-09.tar.gz) | Three jobs passed. Linux measured-row capture was initially the pre-wheel frame, then converged at 100/200 ms in failure diagnostics; the original probe still failed. Clipboard/selection passed unchanged, while three IME groups repeated failures. Later parser/diagnostic fixes require their own run. |

Bundle-02's native surface tests include
`metal_imports_native_bgra_and_nv12_reuses_frames_and_releases_on_teardown` and
`native_surface_component_retains_nodes_fits_padding_and_releases_bindings`.
These test actual imported BGRA/NV12 pixels, cache reuse and lifetime release on
Metal. The native window alternated four frame identities. They are distinct
evidence from PNG rendering, Linux shader validation or Mac cross-compilation.
The independent-device GPU stress test remains opt-in; a suite with that test
ignored is not a stress-test pass.

The early Bundle-02 source archive omitted some root assets, workspace comparison
sources and included documentation. Its manifest/diff and repository base remain
available, but that selective tar alone is not a self-contained build snapshot.
The corrected harness includes those inputs in subsequent source archives.

Run [10](https://github.com/zeronsh/zgui/actions/runs/35813258112) at `16197c6`
passed both core jobs and Mac GPU. Linux passed measured rows, editor selection,
and the corrected native Wayland composition/focus plus external-model/read-only
IME probes. It still failed the clipboard payload check and the older default
synchronous X11 IME probe. Clipboard failed in runs 08/10 and passed unchanged
in 09; its cause remains unresolved. [CI 10](ci-reviewed-10.tar.gz) preserves
full logs. Later runs 11/12 still require their own final outcomes.

## Source and CI correction chronology

The latest source is
[`4e95c8e1c61e74a82df7136617e2e82f377efce8`](https://github.com/zeronsh/zgui/commit/4e95c8e1c61e74a82df7136617e2e82f377efce8).
Production behavior remains the source measured in the final Mac bundle/resource
packets; changes after `7df5c86` are documentation or native-test readiness/trace-parsing, diagnostics and opt-in display-fixture work.
The current full CI run still has to establish its own outcome.

Completed CI packets retain full raw job logs, job/step conclusions, artifact
inventories and available local follow-up diagnostics. They are compressed without
changing their contents; extract neighboring archives to restore their original
`ci-reviewed*` directories and historical relative references. The working raw
copies were moved to `/tmp/zgui-ci-reviewed-raw/`; repository links below use the
immutable archives. An upload step with a warning-only quota failure is not proof
that GitHub retained downloadable artifacts.

| Run / source | Retained packet | Final job outcomes and exact stopping failure |
| --- | --- | --- |
| [35807926924](https://github.com/zeronsh/zgui/actions/runs/35807926924), `475fe92` | [CI 01](ci-reviewed.tar.gz) | Both core jobs reached strict Clippy and failed the new `isolate_lowest_one` lint. Mac GPU bundle failed a fixed-delay multiwindow close assertion. Linux Vulkan/build/native lifecycle passed before missing `xmodmap` stopped the rich/actions/layout/drag step. Overall failed. |
| [35808416547](https://github.com/zeronsh/zgui/actions/runs/35808416547), `05c0cea` | [CI 02](ci-reviewed-02.tar.gz) | Mac GPU job passed. Both core jobs failed Rust 1.98 `chunks_exact_to_as_chunks` Clippy diagnostics; Linux GPU job still stopped on missing `xmodmap`. Local typed-chunk correction attempts/failures and successful follow-up Metal/Clippy logs are retained. Overall failed. |
| [35808887834](https://github.com/zeronsh/zgui/actions/runs/35808887834), `3a0ce13` | [CI 03](ci-reviewed-03.tar.gz) | Both core jobs and Mac GPU passed. Linux passed earlier GPU/native groups, then nested Wayland menu setup failed an immediate host-window regex lookup (`NoneType.group`). Overall failed; the BadAtom line alone does not establish compositor root cause. |
| [35809163293](https://github.com/zeronsh/zgui/actions/runs/35809163293), `8e577cb` | [CI 04](ci-reviewed-04.tar.gz) | Both core jobs and Mac GPU passed; Linux again stopped in native menus/dialogs/file drops with the host-window `NoneType.group` setup failure. Overall failed. |
| [35809509299](https://github.com/zeronsh/zgui/actions/runs/35809509299), `7df5c86` | [CI 05](ci-reviewed-05.tar.gz) | Both core jobs and Mac GPU passed; Linux again stopped at the same nested host-window lookup. Fixture button-order correction did not resolve that distinct readiness failure. Overall failed. |
| [35810106069](https://github.com/zeronsh/zgui/actions/runs/35810106069), `a89d8d0` | [CI 06](ci-reviewed-06.tar.gz) | Both core jobs and Mac GPU passed. Linux passed actual nested menus/dialogs/external-file drops and variable/measured virtual rows, then Wayland input setup failed a `#`-only trace regex despite `xdg_toplevel@23.configure(900, 650)` and `wl_surface@21.attach(wl_buffer@97)`. Overall failed; downstream input actions and later steps were not reached. |
| [35811092921](https://github.com/zeronsh/zgui/actions/runs/35811092921), `46c2c62` | [CI 07](ci-reviewed-07.tar.gz) | Both core jobs and Mac GPU passed. Linux measured-row wheel screenshot sampled the adjacent row color at the first boundary despite passing model assertions; CI 06 had passed the same probe. Stale presentation versus raster boundary behavior is unresolved. Later steps, including the corrected Wayland parser, were not reached. Overall failed; no assertions were weakened. |
| [35811414950](https://github.com/zeronsh/zgui/actions/runs/35811414950), `cca2ead` | [CI 08](ci-reviewed-08.tar.gz) | Both core jobs and Mac GPU passed. Independent Linux probes all ran; six failed: measured virtual rows, Wayland input/clipboard, editor selection, X11 IME, Wayland IME composition, and Wayland external-model/read-only cancellation. Actual `@` preedit/keyboard events reveal two further parser mismatches; X11 post-commit preedit was absent at the component despite visible candidate windows. Investigation remains open; no overall success. |

| Revision | Concrete change / scope |
| --- | --- |
| `475fe92` | Native Mac menu/sheet, application-event, borderless-query and IME repairs, plus validation and retained failure evidence. |
| `05c0cea` | Replace fixed native lifecycle delays with bounded observed-state waits; retain declared Rust 1.96 compatibility while satisfying newer lint guidance. |
| `3a0ce13` | Supply Linux test dependency `xmodmap` and use equivalent typed RGBA chunk iteration for Rust 1.98 Clippy. |
| `8e577cb` | Add optional native active/key-window observer to distinguish real app deactivation from background AX interaction. Final Mac bundle/resource probe source. |
| `7df5c86` | Keep established fixture control positions when adding multi-file/window-state controls; no production behavior change. |
| `92199d5` | Publish the recorded native evidence and explicit remaining gates; documentation commit skipped CI. |
| `a89d8d0` | Tests-only Openbox/nested-Sway readiness helpers wait for real window-manager/compositor state instead of sleeps. Rust/application code unchanged. |
| `a74ac19` | Preserve completed CI failure chronology and native compositor readiness evidence; documentation/evidence only. |
| `46c2c62` | Tests-only compatibility with both Wayland debug object-ID separators, keeping all existing protocol assertions. |
| `cca2ead` | Independent native CI steps continue after unrelated failures while preserving failed job status, so one harness failure does not hide later gate results. |
| `7255f67` | Reviewed opt-in physical-display fixture and measured-row failure diagnostics; production behavior and assertions unchanged. Run [35812238385](https://github.com/zeronsh/zgui/actions/runs/35812238385) failed, with diagnostics retained in [CI 09](ci-reviewed-09.tar.gz). |
| `16197c6` | Remaining Wayland IME/burst parser separators accept `#`/`@`, and failure diagnostics retain exact lines. [Run 10](https://github.com/zeronsh/zgui/actions/runs/35813258112) failed; [CI 10](ci-reviewed-10.tar.gz) retains the outcome. |
| `b6639b8` | Measured-row capture waits for exact asserted raster convergence without weakening model/pixel checks. [Run 11](https://github.com/zeronsh/zgui/actions/runs/35813943395) pending. |
| `4e95c8e` | Owned XIM probe explicitly selects and records supported asynchronous IBus mode; default synchronous limitation remains. [Run 12](https://github.com/zeronsh/zgui/actions/runs/35814061552) pending. |

The [Linux readiness packet](zgui-linux-sway-readiness.tar.gz) records five actual
fresh Xvfb/Openbox/nested-Sway starts in a task-owned official Ubuntu 24.04 ARM64
container. Container identity specifies two CPUs and 2 GiB RAM; image digest,
package versions, helper hash, probe source and per-trial logs/IPC outputs are
retained. Every trial reached an active `X11-1` output. Successful Sway logs still
contain the X11 `op 18:0, code 5` diagnostic (BadAtom), so that diagnostic alone
does not prove failed compositor readiness. The container was removed after the
probe. This checks native compositor readiness only: no Rust build, zgui window,
Linux application acceptance suite or benchmark result is inferred.

## Native UI checklist: all 18 bundle cases

The following outcomes come from live `cua_repl` macOS UI/AX operations, screenshots
inspected during the session, and fixture logs. Framework events were not injected
to simulate OS delivery. They are **separate observations**, not retroactive edits
to Bundle-02's pending checklist. “Observed” describes the listed behavior only;
“Partial” and “Pending” leave the stated acceptance work open.

The [UI archive](zgui-macos-ui-20260922-01.tar.gz) contains chronological
`observations.json`, fixture logs, test data and identities. Several observations
are written records of live AX/screenshots rather than independently archived
screenshot files.

| ID / fixture | Status | Actual observation and remaining qualification |
| --- | --- | --- |
| 00 `platform_services` | Observed | Real AppKit open-file/folder/save results, cancel and alert OK. Owner close dismissed its picker after repair; a later overwrite-confirmation case dismissed both nested sheets, suppressed result delivery and left the sentinel file unchanged. A later actual sheet selected `second.txt` and `first.txt` together, returned exactly those two paths, then fresh Cancel returned `Ok(None)` (`dialog-multiple.log`). Initial single-dialog sequence has UI observations but no reliable fixture log because a symlink bundle attached to another instance. |
| 01 `dynamic_menus` | Observed via UI and native AppKit state | Correct application/File labels after fix; disabled Save suppressed Cmd-S; enabling then selecting Save dispatched; Cmd-R replaced File with Tools; Cmd-D routed new action; Cmd-Q quit. `dynamic-menus-fixed.log`. Later direct NSMenuItem reads verify /File/Enabled checked state 0→1→0 and Save enabled false→true→false, with Cmd-S save and Tools/New command replacement (`dynamic-menus-native-state.log`). This is actual native property proof, not model-state inference; checkmark pixels were not captured. |
| 02 `atspi` | Observed via native AX | Static labels/editor text, button Count 0→1, editor value `Zoë 你好`, selected `你好`, slider 25→60, checkbox off→on, disabled editor rejecting value mutation. Checkbox AX click worked; setting numeric value alone did not toggle. This is macOS AX despite the fixture name, not a full VoiceOver spoken-usability audit. |
| 03 `rich_text` | Observed via UI/AX | Tab/Return activated inline link, AX action activated it again; named link and full unclipped Unicode paragraph exposed. Mixed styles, wrapping and ellipsis visibly rendered. Actual VoiceOver speech was not evaluated. |
| 04 `form` | Observed editing/clipboard subset | TextEdit→zgui and zgui cut→TextEdit preserved exact Unicode clipboard text; cut/undo/redo, multiline Return, Tab focus, resize retention and disabled-field presentation observed. Generic non-ASCII `typeText` lost characters in both TextEdit and zgui, so it is not Unicode keyboard evidence. Native dead-key input is separately verified below; candidate input remains open. |
| 05 `native_ime` commit | Partial: dead key passed, Pinyin pending | Existing Canadian Option-E produced PREEDIT `´`, then E produced exactly one COMMIT `é` and final first-editor `é`. No Pinyin candidate selection/placement was exercised. `ime-deadkey-commit.log`. |
| 06 `native_ime` cancellation | Pending for Pinyin | Escape under the Canadian dead-key source produced COMMIT literal `´`; this is recorded OS behavior, not successful Pinyin cancellation. `ime-deadkey-cancel.log`. Candidate cancellation with unchanged committed model remains unverified. |
| 07 `native_ime` focus | Partial: repaired dead-key editor transfer; native app/Pinyin pending | Before fix, first-editor marked accent followed by second-editor E incorrectly produced second-editor `é`. After input-context discard, second-editor E is plain `e`; fresh Option-E/E then yields `eé`. `ime-deadkey-focus.log`, `-focus-fixed.log`, `-focus-fresh-fixed.log`. Attempted Finder interactions did not establish native deactivation: the final `--native-observe` fixture reports active=true/key=true throughout all 40 ticks (`ime-native-app-observed.log`). The earlier same-editor `é` attempt is inconclusive, not a product failure. Native app-focus reset, Pinyin focus and candidate placement remain unverified. |
| 08 `layout` | Observed | Widths 800/600/1000 retain header/sidebar/main/detail spans; chips reflow 5→3→6 columns and narrow scrolling reaches Item 29. Host reports 2× Retina scale. No physical display-scale transition was performed. |
| 09 `paint_styles` | Observed | Asymmetric edges/radii, two colored shadows, slash pattern and dashed border; gradient changed twice and restored without visible stale pixels or displaced children. |
| 10 `canvas` | Observed | Resize retained path/background layout; changing curve twice restored geometry without old stroke footprints. |
| 11 `svg_transform` | Observed | Rotation and scale visibly changed geometry; twelve rotations/two scale toggles returned to original without visible old footprints. |
| 12 `animated_image` | Native decoded-GIF integration observed | Initial generated-frame pause/resume and minimize/Raise observations are supplemented by current-source GIF-02: real file decode, blue preview paused unchanged for 46.082 seconds, resumed red/green and normal close. Simple full-canvas frames do not establish complex disposal, loop limits, cadence or invisible GPU-submission pause. |
| 13 `window_controls` | Observed via UI and actual AppKit state | Resize to 800×600, maximize and restore observed. Later fixture directly queried NSWindow `isVisible/isMiniaturized/isKeyWindow`: minimized false/true/false; restored true/false/true; hidden false/false/false; shown true/false/true (`window-controls-native-state.log`). These are actual native values, not requested state. CUA's minimized snapshot was cached; hidden AX timed out until show, then recovered. No GPU-submission pause claim follows. |
| 14 `native_platform` | Partial; held-native drag unverified | Real 2× bounds, fullscreen 5120×2880, restored size, minimum clamp and position acknowledgment; root closed and native reopen produced a visible reopened window. OS URL launcher/registered receiver delivered exact `zgui-smoke:accepted` to retained UI. Automation moved neither custom nor standard decorated AppKit titlebars; native diagnostics showed LeftMouseDown/button 0 but pressed buttons 0. A true held gesture is required; this does not confirm a framework drag defect or pass. Later Display-01 observes same-scale physical monitor 0→1→0 and exact restored bounds; mixed-scale/hotplug remain unverified. `native-platform-fixed3.log`, `url-launcher.log`, `native-drag-control.log`. |
| 15 `drag_drop` | Partial | Red/Green/Blue payloads accepted on target; outside drop rejected with unchanged target and no preview after release. Atomic tool drags did not establish held preview/Escape or Finder multi-file drop/cancel/leave behavior. `drag-drop.log`. |
| 16 `actions` | Partial | F5, F1/F2 and x/y saved without accidental insertion; incomplete x replayed on timeout; Escape mismatch replayed x without save; distinct F12 event. Available Mac automation cannot synthesize Insert. Multiple-context/application fallback behavior remains supported by separate behavior tests, not this single-context native fixture. `actions.log`. |
| 17 `cursors` | Unverified native shape | Regions rendered and pointer moved over Move/Disabled; automation screenshot shows its own pointer indicator, so it cannot establish actual macOS cursor shape/restoration. |

The final UI archive contains 48 files and excludes copied `.app` trees and
executables. SHA256:
`977c9d070b802c51d1be8f38d3156bb69ebb959b7d3bd90b9fa16723d89a311f`.
Early failed observations remain alongside final follow-ups, including the
original IME focus failure and inconclusive app-focus attempts. All task fixtures
were closed before this final capture, as reported by the session's process check.

## Repairs and evidence limits

The native menu repair reserves the standard macOS application menu before
application command menus. The owner-sheet repair dismisses descendants before
their parent when closing the owner, including overwrite confirmations. Dropping
an individual dialog future still cancels result delivery without promising sheet
dismissal. The nested native observation, rather than a CLOSED log alone, verifies
physical dismissal.

Application callbacks now use owned AppleEvent handlers while retaining winit's
delegate, and install routes again after native launch initialization. The original
delegate conflict and subsequent borderless maximize-query event storm are
preserved; the sampled stack is in
`native-platform-borderless-event-storm.sample.txt.gz`. Successful fullscreen and
reopen logs follow the repair. The URL test initially failed registration from a
temporary bundle; registering a test receiver in the user's Applications directory
made actual OS launch/delivery work. It did not inject an `ApplicationEvent` or
install framework-wide scheme registration.

Dead-key focus transfer exposed stale state inside AppKit's input context, beyond
clearing the framework's preedit model. The reset repair discarded that native
state, and subsequent real input verified both plain second-editor E and a fresh
accent composition. Canadian dead-key success is narrower than candidate-based
IME qualification. Pinyin input-source changes still need the pending explicit
permission/access decision. A separate request for an explicit native CGEvent
input mechanism is pending for held-mouse, real app-focus and Insert scenarios
that the available automation did not establish. Neither request is treated as
granted; do not bypass the input-tool restrictions while awaiting it.

The custom-titlebar investigation used a standard decorated AppKit titlebar as a
control. Neither moved under the available tool gesture, and native diagnostics
reported no held mouse button. A tentative synchronous drag scope did not help;
its patch is retained separately and is not a production drag repair. The
`--decorated` fixture and event diagnostics remain available for a real held-input
validation. Failed tool gestures are preserved without assigning an unsupported
framework root cause.

The final native focus observer reports the IME application active and its window
key throughout the attempted Finder coordinate-titlebar workflow. Consequently,
the earlier same-editor accented result cannot be classified as stale input after
native deactivation: no such transition was established. Use `--native-observe`
and obtain an actual active/key transition before validating app-focus resets.

Per-run source manifests/diffs, executable identities and toolchain records
identify the automated evidence. UI bundle hashes were captured at specific times
while fixtures were being rebuilt; they cannot retroactively establish the exact
bytes of every earlier UI launch. `bundle-identities.json` states that limitation;
the registered receiver has separate identity metadata. The final capture manifest
also identifies the `native_platform` wrapper as the discarded synchronous drag
experiment, not final production code; the patch is retained separately. Do not label all UI
observations as proof of one final identical binary. The full-suite and focused
quality stages likewise validate their own recorded source states.

## Physical display follow-up

[Display-01](zgui-macos-display-01.tar.gz) uses native `current_monitor`, outer
position and inner-size snapshots rather than requested placement state. The
window moved from monitor 0 at (2080,590) to monitor 1 at (-4920,200), then back
to the original bounds, preserving its 960×640 physical client size. Each
match settled over ten 100 ms observations. Both attached physical displays
reported 5120×2880 and scale 2.0. Normal process exit was 0; strict example
Clippy passed. The packet preserves the new opt-in example and exact executable
identity against unchanged production source `cca2ead`. The included fixture
was untracked at capture and was subsequently delivered in `7255f67`; this
does not retroactively change the packet's recorded HEAD.

This closes same-scale monitor movement/return only. Mixed-scale transitions,
hotplug, sleep/wake, driver loss, held-pointer dragging, pixel restoration and
native cursor appearance remain separate unverified cases.

## Native encoded GIF follow-up

[GIF-02](zgui-macos-gif-02.tar.gz) records source
`cca2ead80d4dd4839d8ace9553263a4bb634ec5d` and executable SHA256
`4dd3af766fbf8182efdeeb624aa019bfbb325297e0de22564392e3682e039e13`.
The input SHA256 is
`29e5251e6f1c438b01531a04f9deb6ac110979cc86763f6f8e0a5aa50a443df9`:
three opaque 120×80 RGB frames, nominal 1300 ms delay, infinite repeat and
Background disposal. Screenshots from the freshly bound `gif_current.app`
wrapper are decisive: its blue preview crop stayed identical for 46.082 seconds;
after resume, red and green appeared, and normal close exited 0. Colors are
color-managed screenshots, not assertions that capture bytes equal source RGB.

The packet retains an earlier exit-137 launch and connection/close ambiguity;
that process later exited 0, and the distinct-wrapper repeat removes reliance
on its uncertain automation connection. [GIF-01](zgui-macos-gif-01.tar.gz) used
older executable bytes matching the prior UI packet; its checkout HEAD was
capture metadata, not build provenance. This follow-up verifies simple native
GIF decoding/presentation/pause integration. Decoder regressions separately
cover partial offsets, disposal modes and loop metadata; no new claim about
cadence, invisible submissions or cancellation bounds follows from screenshots.

## Finite completion gates

| Gate from the capability matrix | Current evidence / remaining work |
| --- | --- |
| Idiomatic public component/children cases | Existing Linux implementation audit and Mac retained fixtures cover delivered APIs. Native fixes preserve retained architecture and fluent styling; this packet does not reopen scope as arbitrary CSS/Zed widgets. |
| Behavior, damage/layout and bounded ownership | Existing Linux tests plus real Mac Metal/CoreVideo tests; final Bundle-04 automated subset passes at `8e577cb`. Sleep/wake and actual driver loss are separate unverified lifecycle cases. |
| Integrated Linux and macOS native behavior | Linux packet retained. Mac partial: all 18 checklist cases are accounted for above across native component executables, rather than one all-in-one gallery launch. Pinyin, actual app-deactivation composition reset, native cursors, Insert, held drag/Escape, Finder multi-file transactions and custom-titlebar/mixed-scale/hotplug behaviors prevent a completion claim. Native dialog multiple-selection and actual AppKit visibility/focus states now have later observed evidence. |
| Matched streaming/list comparison after parity implementation | [36-trial Linux packet](../../results/gpui-parity-workers4/README.md) remains valid for its frozen post-parity source. A matched rerun on stabilized final source remains pending: the continuation includes equivalent shared RGBA iteration changes as well as native Mac fixes. No matched Mac comparison is claimed or substituted for that rerun. The six-trial native resource probe is a distinct smoke measurement. |
| Publish truthful statuses/evidence | Matrix and main docs updated; final UI/repair/bundle and both resource archives retained. Corrective pushed CI still needs its final outcome recorded. Passing counts or model updates do not close missing native gates. |

## Linux XIM compatibility follow-up

[The immutable packet](zgui-linux-native-ime-followup.tar.gz) includes baseline
source `cca2ead`, executable hashes, package versions, raw traces, screenshots,
source patches and the rejected private candidate. IBus 1.5.29-2 (runtime
1.5.29-rc2), libpinyin 1.15.7 and libX11 1.8.7 reproduced the CI failure.
The default synchronous bridge omitted lifecycle signals after commit and at
Escape. Tracing observed XIM updates while winit's composing state was false,
then confirmed absence of raw clear/done callbacks. Removing the commit reset
privately restored preedit but broke Escape, so that candidate was rejected.

Original unpatched binaries with owned `IBUS_ENABLE_SYNC_MODE=0` passed the
unchanged eight-stage Unicode/candidate/Escape/focus probe, external-model and
read-only cancellation plus fresh composition, and engine-off Control-A,
uppercase text, Backspace and ordinary digit recovery. The [official IBus fix](https://github.com/ibus/ibus/commit/719792d300579c1bfdf43251a83c6ed4e5594c07)
adds missing show/hide preedit handling in PostProcessKeyEvent. Delivered
`4e95c8e` selects asynchronous mode only in the owned harness environment and
records mode/version/limitation; no production workaround or default-mode pass
is claimed. Mac Pinyin permission and validation remain separate.

## Native release resource probes

Resource-01's three 10-second idle/active pairs ran with a two-second warmup and no concurrent
builds or other UI validation. All six exited successfully and passed native
GPU/component identity, mounted-row bounds, duration and delivered-model-work
gates. Idle reported zero updates; active rates were 59.9, 59.6 and 59.7 model
updates per requested second. The packet retains 469 post-warmup samples plus
startup observations. These rates do not measure presentation cadence.

| Mode / repeat | CPU, % of one core | Mean RSS, MiB | Mean process physical footprint, MiB |
| --- | ---: | ---: | ---: |
| Idle 0 | 0.0055 | 93.23 | 51.64 |
| Active 0 | 12.8275 | 95.67 | 384.43 |
| Active 1 | 12.9416 | 95.23 | 384.92 |
| Idle 1 | 0.0059 | 93.47 | 183.85 |
| Idle 2 | 0.0322 | 93.31 | 235.86 |
| Active 2 | 14.4490 | 95.51 | 385.08 |

RSS and kernel physical footprint are different accounting measures; the larger
and variable footprint values are retained rather than omitted. The short series
does not explain that variation or establish an allocation/leak bound.

Release executable SHA256:
`531ef71714d7ff4a4c5516b772e1e6dbfc842800515504cdd635daad173da760`.
Resource source archive SHA256:
`25676c9486e70c577dde4127c253976619048337e73eafb8eaeda66f42e55b1c`.
The M5 Mach timebase was 125/3 ns per tick; the native sampler self-check agreed
with `getrusage` (0.050018625 versus 0.050035 seconds). Source-only or fixture-only
edits after this run do not retroactively change its identity.

The final Resource-02 repeated all six trials on rebuilt production source
`8e577cbd2b2bd3065f80a72cea7081fb49a2a5b4`, after the equivalent RGBA-loop change.
All functional/sampling gates passed, with 475 post-warmup samples, zero idle
updates and 59.9 active model updates per requested second in all three active
trials. Builds and other UI validation were paused. Active is the combined
streaming/list workload, not a presented-frame counter.

| Mode / repeat | CPU, % of one core | Mean RSS, MiB | Mean process physical footprint, MiB |
| --- | ---: | ---: | ---: |
| Idle 0 | 0.0077 | 93.65 | 255.59 |
| Active 0 | 13.4368 | 95.53 | 373.34 |
| Active 1 | 13.6802 | 95.51 | 385.25 |
| Idle 1 | 0.0057 | 93.64 | 221.29 |
| Idle 2 | 0.0055 | 93.69 | 205.75 |
| Active 2 | 11.4436 | 95.27 | 378.59 |

Final release executable SHA256:
`92d6a780f5be70e2e4e8f740cbff81fc512948e24d1079d9bd6ea00d9086f8a4`.
Final resource source archive SHA256:
`a66ac370c5862bb22d808a9bc20b7f7c04368c570f7ffa28ad2cfa7ce2a5f8ea`.
CPU-unit self-check: 0.0500207083 seconds from native counters versus 0.050038
seconds from `getrusage`. The archive includes a copy of the release-build log.
Variation between these short runs is not evidence of a performance ranking or
an explained memory trend; both complete series remain available.

Any resource report must retain raw samples and failed trials. CPU uses cumulative
process user/system Mach time converted through the host timebase, with 100% equal
to one core. RSS/physical footprint exclude a complete accounting of GPU and
WindowServer resources. Delivered model updates are not presented frames. The
short probe cannot establish energy, latency, long-run leak freedom or universal
resource minima. Builds and other validation must stop during sampling.
