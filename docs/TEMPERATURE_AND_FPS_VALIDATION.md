# Elevated Temperature and Actual Frame-Rate Validation

**English** | [简体中文](TEMPERATURE_AND_FPS_VALIDATION.zh-CN.md)

Date: 2026-11-11, Hong Kong time. The owner explicitly authorized an elevated read-only temperature test and confirmed UAC. Drivers and the installed service were kept unchanged.

For the subsequent 30 FPS optimization, see [animation frame pacing](ANIMATION_FRAME_PACING.md). This report records the measurements before that optimization.

## New Intel temperature reader

The independent `looppanel-temperature-readonly-test.exe` called `CpuTemperature::open/sample` from commit `0e25fd6`, sampling ten times at 500 ms intervals and sequentially querying the existing service. It did not call display, service-management, or driver-installation paths.

| Sample | New direct reader °C | Existing service °C |
|---:|---:|---:|
| 0 | 47 | 47 |
| 1 | 37 | 37 |
| 2 | 40 | 39 |
| 3 | 35 | 35 |
| 4 | 34 | 34 |
| 5 | 35 | 35 |
| 6 | 35 | 35 |
| 7 | 35 | 35 |
| 8 | 35 | 35 |
| 9 | 35 | 35 |

Opening the reader and all ten samples succeeded. Direct readings ranged from 34 to 47°C; nine matched the service, with a maximum difference of 1°C. The queries were sequential, not simultaneous. The process closed its driver handle and exited with code 0.

The existing `LoopPanelTemperature` service retained process ID 5964, state, binary path, and startup mode. This validates the Intel reading path, not service installation/lifecycle or AMD hardware.

## Frame-rate method

- Isolated copies of main `2badea5` and the fixed branch `0e25fd6` used the same Release settings.
- Timing-only instrumentation recorded the monotonic clock, preparation/transmission duration, and trigger after successful `device.send_packet` calls.
- Each case warmed up for 5 seconds, then measured 30 seconds. Startup's repeated first frames were excluded.
- Records stayed in memory until the measurement window ended.
- Each run held the real global display lock after the original program exited. Frame-rate runners used ordinary privileges.
- The GIF case copied the installed configuration: `fps=30`, the original 102-frame GIF with 50 ms delays (20 FPS source), the original template, Agency FB, brightness 0.90, 1000 ms sensor sampling, and audio spectrum enabled.
- The procedural case removed only the background GIF from the configuration copy and kept `fps=30`.

These counts represent successful complete-frame software sends, not camera FPS or panel scan-out frequency. No USBPcap was used. The original installed binary was not instrumented directly; source builds with equivalent settings were measured.

## Results before optimization

| Case | Successful packets in 30 s | Packet FPS | Animation FPS | Audio-only packets | MB/s |
|---|---:|---:|---:|---:|---:|
| Main + GIF | 590 | 19.67 | 16.17 | 105 | 9.06 |
| Fixed branch + same GIF | 590 | 19.67 | 16.13 | 106 | 9.06 |
| Fixed branch + procedural animation | 768 | 25.60 | 22.37 | 97 | 11.80 |

Audio updates can transmit a new dashboard with the same background frame. Packet FPS is therefore higher than animation advancement. Neither a 20 FPS source nor a 30 FPS limit replaces actual measurement.

| Case | Median send ms | Send P95 ms | Maximum send ms | Median preparation ms | Preparation P95 ms |
|---|---:|---:|---:|---:|---:|
| Main + GIF | 14.455 | 23.511 | 29.697 | 2.007 | 3.755 |
| Fixed branch + GIF | 14.448 | 23.087 | 29.501 | 2.065 | 3.832 |
| Fixed branch + procedural animation | 14.449 | 22.076 | 28.575 | 2.543 | 4.559 |

All runs completed without transmission errors. The fixed branch showed no material regression in this GIF case, but procedural animation did not yet reach 30 FPS. At this stage only measurements were added; scheduling, USB chunks, and protocol were unchanged.

Each case was a single 30-second run under the machine's current load, not a guarantee for every machine or asset.

## Restoration and evidence

The original program was restored as process 288156 and observed through the camera. Original configuration bytes were unchanged, including the owner's 30 FPS limit. The existing service continued running. No drivers or services were installed/replaced, and no remote PR operations occurred during validation.

Local evidence in the validating machine's ignored `target/pr-validation/` directory:

- `temperature-readonly/src/main.rs`: elevated test source.
- `temperature-readonly-results.txt` and `temperature-summary.json`: raw readings and summary.
- `temperature-service-before.json` and `temperature-service-unchanged.json`: service comparison.
- `fps/`: timing source copies, ordinary-user runners, configuration copies, three CSV/log sets, `summary.json`, and restoration record.
- `evidence/15-temperature-fps-restored.png`: restored physical display.
