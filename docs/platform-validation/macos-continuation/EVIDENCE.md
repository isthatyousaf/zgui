# macOS continuation evidence (partial)

This evidence is from an actual Apple Silicon macOS 26.4 desktop. It does **not** establish capability parity or completion of the remaining native gates. `archives.json` records compressed archive hashes; initial Actions diagnostics are in `ci-initial/`.

| Archive | Scope and result | Limits |
| --- | --- | --- |
| `zgui-macos-validation-20260922-01.tar.gz` | Initial bundle failed in GPU tests. Raw failure and results retained. | Original bundle initialized its manual list after GPU tests, so the empty list does not mean manual gates passed. |
| `zgui-metal-fixes.tar.gz` | Before/after logs for bundled-font and platform command modifier fixes; real Metal/CoreVideo tests pass after fixes. | Renderer/model verification does not prove human-facing clipboard, IME or AX behavior. |
| `zgui-macos-validation-20260922-02.tar.gz` | Automated bundle passed: Metal tests, AX projection tests, native surfaces/rich text window probes, component/multiple-window/control lifecycle smoke. Contains source hashes, dirty diff, host/toolchain facts, binary hashes, and logs. | All 18 manual entries remain pending. Control request logs do not alone prove observed native state changes. Source snapshot predates later menu/sheet fixes and has the original selective archive whitelist; use the preserved dirty diff and repository base for missing build inputs. |
| `zgui-macos-quality-01.tar.gz` | Workspace all-target tests, doctests, formatting, strict clippy passed. | Run predates production native app-menu and owner-sheet fixes; those require subsequent targeted verification. |
| `zgui-macos-quality-02.tar.gz` | Workspace all-target tests, doctests, formatting and strict clippy pass after reviewed native menu and recursive owner-sheet dismissal fixes. Includes working-tree diff, base commit and toolchain identity. | Native interaction observations are recorded separately; passing tests alone is not parity proof. |
| `zgui-macos-quality-03.tar.gz` | Final workspace all-target tests, doctests, formatting, strict clippy and release workload build pass after AppleEvent ownership and borderless maximize-query fixes. Source manifest, diff and release executable SHA256 retained. | Read alongside actual OS URL/reopen/fullscreen observations and resource probe; tests alone do not close those gates. |
| `zgui-macos-quality-04.tar.gz` | Final IME reset follow-up: formatting, focused native IME tests, strict workspace clippy, release workload build pass. Resets include editor changes, explicit cancellation, and host deactivation. Final source/binary hashes retained. | Quality-03 supplies the immediately prior full-suite baseline; actual dead-key focus regression observations are separate native UI evidence. |

Later completed packets are listed below. The [current overview](README.md)
contains all 18 native checklist cases and their precise qualifications. No
manual pass is inferred from automated suites; actual native observations remain
separate, including the unverified candidate-based IME and held-input cases.

| Archive | Scope and result | Limits |
| --- | --- | --- |
| `zgui-macos-validation-20260922-03.tar.gz` | Complete automated bundle passed after IME context reset; corrected source archive includes required assets/workspace/docs inputs. | Predates the later equivalent RGBA-loop lint change. |
| `zgui-macos-validation-20260922-04.tar.gz` | Complete automated bundle passed at `8e577cb`, including final production code and observer fixtures. | Pending GUI cases are not automated passes. |
| `zgui-macos-resources-01.tar.gz` | Six release native resource trials passed delivered-work and sampling gates; raw CPU/RSS/physical-footprint data retained. | Not a matched comparison, presentation metric or leak proof; later resource rerun is separate. |
| `zgui-macos-resources-02.tar.gz` | Final six-trial release process-resource probe at `8e577cb` passed all gates; release-build log copied into packet, original retained. | Measurements belong to this binary/source, with both RSS and larger physical-footprint values reported; no matched hardware ranking or presentation/energy claim. |
| `zgui-macos-ui-20260922-01.tar.gz` | Final 48-file UI packet: observations, logs, fixture data and capture-time identities; all application bundles/executables excluded. | Native app deactivation was not established; Pinyin and held-input cases remain open. Hashes at capture time cannot prove all earlier launch bytes. |
| `zgui-metal-fixes-final.tar.gz` | Final 36-file repair/experiment log packet including reverted synchronous-drag patch. Earlier archive unchanged. | Discarded experiment is not a delivered drag fix; current native-platform wrapper identity records that distinction. |
| `zgui-macos-fixture-order-followup.tar.gz` | Three later formatting/build/Clippy logs and commit identity/patch at `7df5c86`, preserving existing fixture button coordinates after adding validation controls. | Fixture-only follow-up; frozen prior archives unchanged, no new UI pass inferred. |
| `zgui-linux-sway-readiness.tar.gz` | Five fresh native Xvfb/Openbox/Sway readiness trials passed in an official Ubuntu 24.04 ARM64 container bounded to two CPUs/2 GiB, with active `X11-1` output each time. Image/packages/helper/probe and per-trial logs retained; task container removed. | No Rust applications exercised. Successful logs still show X11 code 5 (BadAtom); do not infer compositor failure from that line alone. Later CI-06 exercised real application paths separately. |

Completed CI logs are now stored as the immutable packets below; original
`ci-reviewed*` directory layouts remain inside each archive. Extract packets next
to one another to resolve their historical relative links. Working raw copies
were moved to `/tmp/zgui-ci-reviewed-raw/`; completed CI-06 is also archived below.

| Archive | Run/source and final outcome | Limits |
| --- | --- | --- |
| [ci-reviewed.tar.gz](ci-reviewed.tar.gz) | `35807926924` / `475fe92`: both core Clippy jobs fail `isolate_lowest_one`; Mac bundle fixed-delay lifecycle assertion fails; Linux lacks `xmodmap`. | Earlier successful steps do not make this run green. |
| [ci-reviewed-02.tar.gz](ci-reviewed-02.tar.gz) | `35808416547` / `05c0cea`: Mac GPU passes; both core Clippy jobs fail typed-chunk lint, Linux still lacks `xmodmap`. | Local failed correction attempts and subsequent GPU/Clippy success are separate from remote outcomes. |
| [ci-reviewed-03.tar.gz](ci-reviewed-03.tar.gz) | `35808887834` / `3a0ce13`: both core jobs and Mac GPU pass; Linux nested menu setup fails host-window regex lookup. | BadAtom diagnostic alone does not explain that failure. |
| [ci-reviewed-04.tar.gz](ci-reviewed-04.tar.gz) | `35809163293` / `8e577cb`: three jobs pass; Linux repeats host-window lookup failure. | No complete Linux acceptance run. |
| [ci-reviewed-05.tar.gz](ci-reviewed-05.tar.gz) | `35809509299` / `7df5c86`: three jobs pass; Linux repeats host-window lookup failure. | Fixture button-order fix is distinct from readiness correction. |
| [ci-reviewed-06.tar.gz](ci-reviewed-06.tar.gz) | `35810106069` / `a89d8d0`: three jobs pass; Linux real nested menus/dialogs/file drops and variable/measured rows pass, then Wayland input setup rejects valid `@` trace IDs. | Correct configure/attach events are retained. Downstream input and remaining steps were not reached; this run still failed overall. |
| [ci-reviewed-07.tar.gz](ci-reviewed-07.tar.gz) | `35811092921` / `46c2c62`: both core jobs and Mac GPU pass; Linux measured-row wheel screenshot differs at a row boundary, though model assertions pass. | CI 06 passed the same probe. Stale presentation versus raster behavior remains unresolved; later steps were not reached and this run failed overall. |
| [ci-reviewed-08.tar.gz](ci-reviewed-08.tar.gz) | `35811414950` / `cca2ead`: both core jobs and Mac GPU pass; six independent Linux probes fail. | Measured rows, Wayland input/clipboard, editor selection, X11 IME and two Wayland IME groups require follow-up. All independent steps ran; complete failure logs are retained. |
| [ci-reviewed-09.tar.gz](ci-reviewed-09.tar.gz) | `35812238385` / `7255f67`: three jobs pass; Linux measured rows and three IME groups fail. | Failure diagnostics show the measured initial frame was pre-wheel; later frames converge without making the original probe pass. Clipboard and selection passed unchanged. |
| [ci-reviewed-10.tar.gz](ci-reviewed-10.tar.gz) | `35813258112` / `16197c6`: three jobs pass; Linux clipboard payload and old synchronous X11 IME fail. | Measured rows, selection and corrected Wayland composition/focus/model/read-only IME probes pass. Clipboard failed 08/10 and passed unchanged 09; its cause remains unresolved. |

`46c2c62` retains all protocol assertions while allowing both `#` and `@`
object IDs. `cca2ead` also runs independent native checks after unrelated
earlier failures while preserving failed job status; its
[run outcome](https://github.com/zeronsh/zgui/actions/runs/35811414950) is failed, as recorded above.
Latest `7255f67` delivers the reviewed opt-in display fixture and measured-row
failure diagnostics without production changes; its
[run 09](https://github.com/zeronsh/zgui/actions/runs/35812238385) also failed as recorded above.

Initial CI had: a macOS test using Control instead of Command; Linux linker SIGBUS with unresolved root cause; and account artifact quota blocking both evidence uploads. Workflow improvements preserve textual diagnostics and bound builds, but a successful fresh CI run is still required.

Native GIF follow-up archives preserve the earlier wrapper experiment
[GIF-01](zgui-macos-gif-01.tar.gz) and decisive current-source
[GIF-02](zgui-macos-gif-02.tar.gz). The distinct wrapper built at `cca2ead`
paused identical blue preview pixels for 46.082 seconds, resumed red/green and
closed with exit 0. Source, input and executable identities plus captures are
retained. This closes simple file decoding/native playback/pause integration,
not complex disposal, finite loops, cadence or invisible submission bounds.
Earlier launch and connection ambiguity remains in the same chronology.

[Linux XIM diagnosis](zgui-linux-native-ime-followup.tar.gz) preserves actual
default-mode failures and private discarded candidates, followed by unpatched
asynchronous-mode Pinyin/candidate/cancellation and ordinary-input passes.
IBus 1.5.29 default synchronous compatibility remains a limitation; the scoped
harness change in `4e95c8e` is not a production fix. Source, package and binary
identities plus all probes/screenshots are retained.
