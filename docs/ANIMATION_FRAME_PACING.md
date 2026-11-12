# Animation Frame Pacing and Validation

**English** | [简体中文](ANIMATION_FRAME_PACING.zh-CN.md)

Date: 2026-11-12, Hong Kong time. Branch: `codex/pr-integration-fixes`. Baseline: `e9ce727`.

## Result

On the validation machine, procedural animation improved from approximately 22.38 FPS to 30.00 FPS. Two final 60-second runs each transmitted 1,800 animation-advancing frames; every five-second window was between 29.8 and 30.2 FPS.

The production change affects only the next animation deadline in `src/display.rs`. Drivers, USB protocol/chunking/startup, sensors, audio sampling, and one-second keep-alive behavior were unchanged. No busy waiting, thread-priority increase, or system timer-resolution change was introduced.

This is stable window-average throughput, not hard real-time frame spacing. Frame-interval P95 remained approximately 47.5–47.6 ms.

## Diagnosis and controlled experiment

Three hypotheses were considered: late wake-ups accumulating in relative deadlines, audio-only packets consuming animation time, and preparation or USB transmission exceeding the frame budget.

The baseline animation-trigger lateness median was approximately 12.97 ms; median transmission was 14.43 ms and preparation 3.68 ms. The old `now + interval` calculation carried the current late wake-up into the next deadline, repeatedly lengthening the intended 33.3 ms cadence.

The single-variable experiment changed only deadline calculation:

- Normally advance from `previous_deadline + interval`.
- If that new deadline has already arrived or passed, restart from `now + interval`. Advance one frame per iteration rather than queuing a catch-up burst.

The first 30-second comparison improved from 22.50 to 30.03 FPS while median USB transmission stayed at 14.435 / 14.434 ms. Sensors and audio remained enabled. Accumulated deadline drift was the demonstrated main cause; changing the transport was unnecessary for this target.

## Method and acceptance criteria

A production-equivalent timing build recorded timestamps, preparation/transmission duration, wake-up lateness, and trigger after each successful complete-frame send. Samples stayed in memory until the window ended. Startup frames were excluded and each run warmed up for five seconds.

Both versions used the same Intel CPU, NVIDIA GPU, TM-360, template, Agency FB, brightness 0.90, 1000 ms sensor sampling, audio spectrum, and `fps=30`. The procedural case removed the GIF only from a configuration copy.

Before the controlled runs, the acceptance script required:

- Average animation FPS within 0.5 of the target.
- Every five-second window within 1 FPS of the target.
- Frame-interval P95 at most 1.5 target periods, and the maximum interval at most 3 periods.

These criteria measure stable throughput under normal local load, not hard real-time behavior.

## Before and after

| Case | Window | Animation frames | Animation FPS | Five-second FPS range | Complete-packet FPS |
|---|---:|---:|---:|---:|---:|
| Before, procedural | 60 s | 1,343 | 22.38 | 22.0–22.8 | 25.73 |
| After, procedural, run 1 | 60 s | 1,800 | 30.00 | 30.0–30.0 | 31.87 |
| After, procedural, run 2 | 60 s | 1,800 | 30.00 | 29.8–30.2 | 32.00 |
| After, original 20 FPS GIF | 30 s | 600 | 20.00 | 20.0–20.0 | 22.70 |

Packet counts include audio-only updates and must not be treated as animation FPS. The GIF retained its 50 ms source delays instead of being accelerated to 30 FPS.

| Case | Median animation interval ms | Interval P95 ms | Maximum interval ms | Median send ms | Send P95 ms |
|---|---:|---:|---:|---:|---:|
| Before, procedural | 45.931 | 49.278 | 62.034 | 14.428 | 21.930 |
| After, run 1 | 31.529 | 47.477 | 54.009 | 14.425 | 17.939 |
| After, run 2 | 31.743 | 47.609 | 52.034 | 14.424 | 17.074 |
| After, GIF | 47.215 | 63.835 | 71.715 | 14.435 | 23.220 |

All runs ended normally without transmission errors. Both final procedural runs and the GIF regression passed the predefined criteria; the baseline failed. Counts came from successful software sends, not the camera or the panel's scan-out rate.

## Regression checks

The old calculation was first extracted into the actual production call path. `late_animation_wakes_do_not_accumulate_frame_drift` failed with an extra 13 ms on the first deadline, then passed after the fix.

Three offline tests cover:

- Three hundred cycles of 13 ms late wake-ups without accumulated drift.
- Restarting after a whole missed period or exact boundary.
- Variable GIF delays and the FPS limit preserving the intended schedule.

Formatting, all-target offline tests, Clippy with denied warnings, and both Release binaries passed. The suite had 63 passing offline tests and one explicitly invoked GPU hardware test ignored by default. That GPU path had already passed hardware validation and was unchanged here.

Standards and specification reviews found no blocking issue. Sustained system load or work exceeding the frame budget can still prevent an ordinary desktop thread from delivering every frame at exactly 33.3 ms.

## Restoration and local evidence

The original program was restored as process 333008 and observed through the camera. Original configuration bytes were unchanged, retaining `fps=30`; the existing temperature service remained process 5964. Drivers/services and remote PRs were not changed during validation.

The validating machine's ignored `target/pr-validation/fps-optimization-20261112/` directory contains timing source copies, runners, acceptance script, per-run CSV/JSON/logs, and the four check logs. For example:

```powershell
node target/pr-validation/fps-optimization-20261112/analyze.mjs target/pr-validation/fps-optimization-20261112/optimized-long-1.csv 60 30 --require-target
```

Timing hooks remained in the isolated test copies; production commits contain only the scheduling fix and tests.
