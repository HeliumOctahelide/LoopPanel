# Architecture

**English** | [简体中文](ARCHITECTURE.zh-CN.md)

LoopPanel separates a reusable display pipeline from the transport currently validated for the TM-360.

```text
Configuration + PNG/JPEG/GIF/procedural animation
                   |
                   +-- centered 480x270 background or full-screen large-text background
                   |
Windows sensors -- MiniJinja -- SVG -- resvg -- cached RGBA dashboard
                   |
                   +-- WASAPI -- Hann window -- FFT -- 16 spectrum bands
                                                 |
                                          480x480 RGBA
                                                 |
                                      limited-range UYVY422
                                                 |
                                       MS9132 header + trailer
                                                 |
                                  HID control + libusb0 EP04 bulk OUT
```

## Modules

| File | Responsibility |
|---|---|
| `config.rs` | TOML loading, relative paths, value limits, and safe localization of bundled comments |
| `media.rs` | PNG/JPEG/GIF, procedural animation, and background placement |
| `render.rs` | Typed MiniJinja context, SVG rasterization, alpha compositing, and spectrum drawing |
| `audio.rs` | WASAPI loopback, mix-format decoding, FFT, 16 bands, and smoothing |
| `monitor.rs` | CPU topology/load/frequency, memory, GPU backend selection, and I/O snapshots |
| `adl.rs` | Radeon adapter selection and ADL performance-log decoding |
| `windows_gpu.rs` | PDH GPU load/memory counters and D3DKMT memory totals/temperature when NVML and ADL are unavailable |
| `io_metrics.rs` | IP Helper network counters and PDH disk rates |
| `temperature*.rs` | PawnIO CPU temperature client and minimal service: Intel Package and AMD Tctl/Tdie |
| `protocol.rs` | 480x480 RGBA to UYVY422 conversion and frame packaging |
| `transport.rs` | TM-360 enumeration, HID startup, bulk chunking, and frame transmission |
| `display.rs` | Sensor, dashboard, spectrum, animation, rotation, reload, and keep-alive scheduling |
| `tray.rs` | GUI entry point, first-run UAC service setup, Win32 tray, worker lifecycle, and exit |
| `startup.rs` | Start-at-login registration in HKCU |
| `settings.rs` | Layout, rotation, temperature unit, and background preferences stored in HKCU and shared with the display thread |
| `i18n.rs` and `locales/*.toml` | Embedded message catalogs selected by Windows UI language, with English fallback |
| `process.rs` | JONSBO-AIO/LCD Fusion conflict checks and the global display lock |

GPU backend priority is NVML, then ADL, then Windows counters.

## Privilege boundary

The tray, templates, media, monitoring, and USB display process run as the normal user. Windows does not expose the required Intel Package or AMD Tctl readings to ordinary users, so temperature access uses a separate LocalSystem service.

If the service is missing, the GUI requests UAC through `ShellExecuteExW`, waits for installation, and verifies an ordinary-user pipe read. Later launches reuse the service without elevation. The service exposes one fixed sampling request and a temperature response; clients cannot supply MSR/SMN addresses or write requests.

At startup the service selects IntelMSR or AMDFamily17 by CPUID vendor and family. An AMD SMN read writes the fixed index and reads its data, so each sample holds `Global\Access_PCI` to serialize access with cooperating monitoring software.

## Scheduling and content changes

- Sensors default to 1 Hz.
- Scheduled SVG dashboard refreshes occur at up to 1 Hz.
- The audio spectrum updates at 5 Hz.
- Animation follows the GIF's frame delays or the configured FPS limit.
- When content is unchanged, the cached frame is resent every second, matching the validated TM-360 keep-alive behavior.

Only changed content is composited and encoded. Static backgrounds do not redraw at the configured animation FPS.

Animation deadlines advance from the previous planned deadline, so late wake-ups do not accumulate as permanent drift. If a full period has already been missed, the next period starts from the current time instead of queuing a burst of old frames. See [frame-pacing validation](ANIMATION_FRAME_PACING.md).

Reloads and layout changes prepare the configuration, renderer, background, and encoded frame before replacing the active content. Failed changes retain the valid display. Unit-only changes reuse accepted resources and preserve GIF progress. Successful reloads schedule an immediate sensor sample.

## Dynamic libraries

Windows libraries and driver interfaces are loaded from the system directory, including `hid.dll`, `setupapi.dll`, `libusb0.dll`, `nvml.dll`, `atiadlxx.dll`, `gdi32.dll`, `iphlpapi.dll`, and `pdh.dll`.

Unavailable monitoring features produce `none` values. Missing HID/libusb0 dependencies required for the display produce an explicit error.
