# Template Guide

**English** | [简体中文](TEMPLATES.zh-CN.md)

A LoopPanel template is a MiniJinja text file that renders to valid SVG. The default canvas and current transport are fixed at 480x480.

## Basic rules

- The default filename is `dashboard.svg.jinja`.
- Declare `width="480" height="480" viewBox="0 0 480 480"` on the root element.
- Undefined variables are errors.
- Dynamic text is XML/HTML escaped automatically; do not mark user text as `safe`.
- Optional readings are `none`. Check before formatting; zero is valid, so use `is not none` rather than a truthiness test.
- Only the configured font file is loaded. SVG `sans-serif` maps to that font.
- Relative SVG resource paths are resolved against the template directory.

## Complete context

### Top level

| Expression | Type | Meaning |
|---|---|---|
| `title` | string | Configured title |
| `time` | string | Local time, `HH:MM:SS` |
| `show_clock` | bool | Configured clock switch |
| `show_sensors` | bool | Configured sensor switch |
| `temperature_unit` | string | Tray selection: `C` or `F` |
| `custom_lines` | list[string] | At most two entries |

### CPU

| Expression | Type |
|---|---|
| `cpu.load_percent` | float, 0–100 or `none`; Windows Processor Utility, matching Task Manager's overall metric |
| `cpu.temperature_c` | integer or `none`; Celsius: Intel Package or AMD Tctl/Tdie |
| `cpu.temperature` | integer or `none`; display value converted to `temperature_unit` |
| `cpu.performance_frequency_ghz` | float or `none` |
| `cpu.efficiency_frequency_ghz` | float or `none` |
| `cpu.performance_cores` | list of `{ opacity }`, at most 8 entries |
| `cpu.efficiency_cores` | list of `{ opacity }`, at most 16 entries |

Core `opacity` is mapped to 0.18–1.0 and can be used directly as SVG `fill-opacity`. It comes from per-core busy time, a different metric from overall Processor Utility.

### Memory

| Expression | Type |
|---|---|
| `memory.used_gib` | float |
| `memory.total_gib` | float |
| `memory.used_percent` | float, 0–100 |
| `memory.bar_width` | float, 0–230 |

### GPU

LoopPanel tries NVIDIA NVML, AMD ADL, then Windows PDH/D3DKMT (for example, for Intel graphics). If none is available, all GPU fields are `none`.

The Windows backend provides load, temperature when reported by the driver, and memory. It treats adapters with less than 512 MiB of dedicated memory as integrated and reports shared memory; this is a heuristic. Its load metric sums processes per physical engine and selects the busiest engine.

On AMD, `gpu.power_limit_w` and `gpu.performance_state` remain `none`. Power prefers board power and otherwise uses ASIC power; temperature prefers edge temperature and otherwise uses hotspot temperature.

| Expression | Type |
|---|---|
| `gpu.load_percent` | integer or `none` |
| `gpu.temperature_c` | integer or `none`; Celsius |
| `gpu.temperature` | integer or `none`; converted display value |
| `gpu.power_w` | float or `none` |
| `gpu.power_limit_w` | float or `none` |
| `gpu.memory` | object or `none` |
| `gpu.memory.used_gib` | float |
| `gpu.memory.total_gib` | float |
| `gpu.memory.bar_width` | float, 0–230 |
| `gpu.graphics_clock_mhz` | integer or `none` |
| `gpu.memory_clock_mhz` | integer or `none` |
| `gpu.performance_state` | integer or `none` |
| `gpu.fan_percent` | integer or `none` |

### I/O

These fields are floats or `none`, in MiB/s:

- `io.network_down_mib_s`
- `io.network_up_mib_s`
- `io.disk_read_mib_s`
- `io.disk_write_mib_s`

Use `temperature` with `temperature_unit` for displayed temperatures. Keep color thresholds in `temperature_c`, which always remains Celsius.

## Built-in layouts and reloads

Selecting a layout other than Default uses an embedded template instead of the configured `template`, and does not open an audio endpoint:

- `simple.svg.jinja`: CPU temperature and load.
- `cpu_gpu.svg.jinja`: temperature and load for CPU and GPU.
- `temperatures.svg.jinja`: CPU and GPU temperatures.
- `clock.svg.jinja`: time with CPU/GPU temperatures below.

Built-in layouts keep sampling and redrawing regardless of `show_clock` or `show_sensors`. The configured image/GIF fills the panel behind a dimming layer. The tray background switch can replace it with a plain background.

Automatic rotation follows menu order. The default template is read when switching to Default. Reloads and layout changes finish loading, rendering, and frame encoding before accepting new content. A failure is reported while valid content continues. Unit changes reuse the accepted renderer without reading the disk template or resetting the GIF. Successful reloads schedule a fresh sensor sample immediately.

## Common syntax

```jinja
{{ "%.1f"|format(memory.used_gib) }}

{% if cpu.temperature_c is not none %}
  {{ "%d"|format(cpu.temperature_c) }}°C
{% endif %}

{% for core in cpu.performance_cores %}
  <rect x="{{ 20 + loop.index0 * 24 }}" y="80" width="18" height="12"
        fill="#16bde8" fill-opacity="{{ core.opacity }}"/>
{% endfor %}
```

Temperature colors can be expressed directly in the template:

```jinja
fill="{% if cpu.temperature_c is none %}#7f8790{% elif cpu.temperature_c >= 85 %}#d74242{% elif cpu.temperature_c >= 71 %}#e46d32{% elif cpu.temperature_c >= 56 %}#d29a26{% else %}#25a779{% endif %}"
```

## 16-band audio spectrum

Add this exact marker:

```xml
<g id="audio-spectrum"/>
```

It enables WASAPI loopback capture. To avoid reparsing and rasterizing the whole SVG every 200 ms, Rust draws the spectrum during final compositing. Position, block size, and colors are the `AUDIO_*` constants in `src/render.rs`. Removing the marker disables the endpoint and spectrum refreshes.

The spectrum represents frequency energy from the default playback endpoint, not peak volume. It uses the actual mix format (such as 48 kHz), a 1024-point Hann-windowed FFT, 16 nonempty logarithmic bands, and independent attack/decay smoothing.

## Refresh rates

- Sensors follow `sensor_interval_ms`.
- Scheduled SVG dashboard refreshes occur at up to 1 Hz.
- Spectrum data updates every 200 ms and is composited with the cached dashboard.
- GIF frame delays are retained and constrained by the `fps` limit.
- Static backgrounds redraw for dashboard/spectrum changes; an idle display still resends the cached frame once per second.

After editing a template, choose **Reload settings**. Errors retain the valid display. Run the rendering unit tests before submitting changes, and check text bounds, transparency, and the live spectrum during an authorized hardware test.
