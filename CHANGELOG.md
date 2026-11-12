# Changelog

## 0.1.0-rc.4 — 2026-11-12

### Added

- AMD Zen CPU temperature and Radeon GPU readings, plus a Windows GPU-counter backend when NVML and ADL are unavailable.
- Four large-text layouts: Simple, CPU + GPU, Temperatures, and Clock.
- Automatic layout rotation, Celsius/Fahrenheit selection, and a background switch for large-text layouts.
- Reload settings and templates from the tray without restarting the display.
- Simplified Chinese, English, and Korean messages and localized bundled configuration comments.
- Per-Monitor V2 DPI awareness for sharper native menus.
- English as the primary README and documentation language, with Chinese translations and a Korean README.

### Fixed

- Configuration localization preserves multiline values and protects the original file against partial writes and replacement failures.
- Failed reloads and layout switches preserve the valid display, including during automatic rotation.
- Sampling interval changes apply immediately; reselecting the same layout jumps back and restarts rotation timing.
- Temperature-unit changes reuse accepted resources and preserve GIF progress.
- Windows GPU utilization is aggregated per physical engine.
- Animation deadlines retain their cadence instead of accumulating wake-up delays. Two 60-second hardware runs reached 30.00 animation FPS with a 30 FPS limit.
- Tray menu placement and foreground behavior, and startup on systems without the Office-only Agency FB font.

Thanks to @kkhunter2 for PRs #1–#8 and the hardware testing.

## 0.1.0-rc.3 — 2026-08-26

- Use Windows Processor Utility for overall CPU load.
- Show unavailable CPU counters as missing instead of zero.
- Copy the default dashboard template into Cargo build output.
