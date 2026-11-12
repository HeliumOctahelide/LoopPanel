# Integration and Validation of Eight PRs

**English** | [简体中文](PR_INTEGRATION_VALIDATION.zh-CN.md)

Date: 2026-11-11, Hong Kong time. Validation branch: `codex/pr-integration-fixes`.

## Sources and scope

The isolated worktree started at PR #8, `3bfd4ac9481fa1e38420bfccda64298d002bd1b8`, retaining the original #1 → #2 → #3 → #5 → #6 → #7 → #8 commits. PR #4's independent commit, `1f507b3f7f594b72a67f0c0c1233ce9f7b2f3ccd`, was cherry-picked with its source reference as `2e52a0e`.

The owner authorized stopping LoopPanel, running test builds, and observing the physical display through a camera. Changes and helpers were confined to the worktree or ignored test directories. During validation, the main branch and original installation configuration were preserved; drivers and the installed temperature service were not changed. No remote PRs were merged or commented on during this validation phase.

## Fixes

1. Use the existing dependency-tree TOML lexer to localize only standalone comments, preserving multiline strings and trailing comments.
2. Copy the original configuration to a same-directory temporary file, preserving Windows security attributes; write and synchronize it, then replace it with a single rename. Read-only files, partial writes, and replacement failures preserve the original.
3. Aggregate Windows GPU processes per physical adapter/engine, then select the busiest engine instead of adding distinct engines of the same type.
4. Reschedule sensor sampling immediately after a successful reload.
5. Represent an explicit layout selection separately, so selecting the same item during rotation jumps back and restarts the timer.
6. Prepare rendering content and the complete encoded frame before committing a reload or layout switch. Failed changes preserve valid content and do not terminate rotation.
7. Unit-only changes reuse accepted resources; errors restore the previous unit, and GIF progress is preserved.

The real GPU query test is an explicitly invoked hardware test. Rust 1.99's Clippy required the equivalent `as_chunks::<8>()` spelling for a pre-existing pixel loop; captured-protocol tests still pass. TM-280 wording was narrowed to the contributor's single-unit report pending full acceptance evidence.

## Offline checks

Toolchain: Rust/Cargo 1.99.1, `x86_64-pc-windows-gnullvm`, rustfmt, clippy, rust-mingw, and rust-lld. Locked dependencies were built offline.

| Check | Result at this validation stage |
|---|---|
| `cargo fmt --all -- --check` | Passed |
| `cargo test --all-targets --locked --offline` | 60 passed; 1 hardware test reserved for explicit execution |
| `cargo clippy --all-targets --locked --offline -- -D warnings` | Passed |
| `cargo build --release --bins --locked --offline` | Passed |
| `git diff --check` on the fixes | Passed |

Eight new regression tests cover multiline strings, partial-write failure, replacement failure/read-only files, distinct GPU engines, repeated layout selection, reload scheduling, failed reload/rotation, and unit changes using accepted resources.

Before the fixes, two offline counterexamples failed: GPU aggregation returned 100 instead of 40, and localization changed the multiline `title` value. Both passed after the fixes. Reload preparation was separated from USB execution into `DisplayContent`, used by both production and offline tests.

## Hardware validation

Environment: the owner's Intel CPU + NVIDIA GPU + TM-360. The existing Aoni HD Camera page reported 640x480 at approximately 17 FPS.

The isolated `LoopPanel.exe` first displayed the existing GIF, CPU/GPU readings, load, and audio spectrum. After a normal tray-process exit, a temporary controller called the same production `display::run_until` and `DisplaySettings` APIs while holding the real global display lock. The controller was not added to the release entry points.

| Scenario | Observed result |
|---|---|
| Default, Simple, CPU + GPU, Temperatures, Clock | All displayed correctly |
| Large-text layouts with `show_sensors=false` and `show_clock=false` | Readings continued changing |
| Celsius/Fahrenheit, three-digit values, plain background | Correct and readable |
| Reselecting Simple after automatic rotation | Jumped back immediately |
| Successful font, brightness, template, and sampling reload | RGB bars and a `RELOAD OK` card appeared; clock continued updating |
| Invalid TOML reload | Rejected; the old card's clock continued |
| Invalid Jinja reload followed by a full rotation | Rejected again on returning to Default; later layouts continued without ending the display thread |
| Unit change with an invalid disk template | Accepted display changed from F to C |
| Normal controller shutdown | `DISPLAY_EXIT result=Ok(()) rejected=4` |
| Original version restored | Original display returned; configuration bytes and saved display options were preserved |

The explicitly invoked Windows GPU query test passed on the NVIDIA adapter. One sample returned about 4% load, 37°C, 2,972,549,120 bytes used, and 16,753,098,752 bytes total.

Inspection of the final PE resource directory found manifest type 24, ID 1, in the tray binary, containing `PerMonitorV2, PerMonitor` and `asInvoker`. The service binary had no such manifest.

## Validation boundaries

- The initial ordinary-user attempt to open the new Intel PawnIO reader returned `0x80070005`. A later owner-authorized elevated read-only test passed ten reads; see [temperature and frame-rate validation](TEMPERATURE_AND_FPS_VALIDATION.md). The installed service was not replaced, so its new installation/lifecycle was not exercised.
- This machine has no AMD CPU/Radeon or Intel GPU; those combinations were not hardware-validated here.
- The controller exercised production settings/display APIs, not every tray mouse/keyboard interaction or error dialog. Mixed-DPI multi-monitor behavior was not tested.
- Camera tilt, reflections, and occasional exposure overlap permit layout, refresh, and recovery checks, not precise color measurement.

## Local evidence

The validating machine's ignored `target/pr-validation/` directory contains check logs, the controller source/log, configuration backups, manifest checks, and camera images. Key images: `09-reload-success.png`, `10-invalid-config-keeps-running.png`, `11-rotation-survives-bad-template.png`, `12-unit-switch-ignores-bad-disk-template.png`, and `13-original-app-restored.png`.

The standards and specification reviews both rechecked their identified integration regressions; neither found a remaining blocking issue.
