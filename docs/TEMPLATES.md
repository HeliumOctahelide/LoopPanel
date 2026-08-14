# LoopPanel 模板指南

LoopPanel 的模板是一个 MiniJinja 文本文件，渲染结果必须是有效的 SVG。默认画布是 480×480；当前设备传输层也固定为这一尺寸。

## 基本规则

- 模板文件默认名为 `dashboard.svg.jinja`；
- 根节点应声明 `width="480" height="480" viewBox="0 0 480 480"`；
- 未定义变量采用严格错误模式；
- 动态文本按 XML/HTML 规则自动转义；不要对用户文本使用 `safe`；
- 可选数值是 `none`，应先判断再格式化；零是有效值，不要用普通真假判断代替 `is not none`；
- 程序只加载配置指定的一份字体，并把 SVG 的 `sans-serif` 映射到该字体；
- 相对的 SVG 资源路径以模板目录为基准。

## 完整上下文

### 顶层

| 表达式 | 类型 | 含义 |
|---|---|---|
| `title` | string | 配置中的标题 |
| `time` | string | 本地时间，格式 `HH:MM:SS` |
| `show_clock` | bool | 配置中的时钟开关 |
| `show_sensors` | bool | 配置中的传感器开关 |
| `custom_lines` | list[string] | 最多两项 |

### CPU

| 表达式 | 类型 |
|---|---|
| `cpu.load_percent` | float，0–100 |
| `cpu.temperature_c` | integer 或 `none` |
| `cpu.performance_frequency_ghz` | float 或 `none` |
| `cpu.efficiency_frequency_ghz` | float 或 `none` |
| `cpu.performance_cores` | list of `{ opacity }`，最多 8 项 |
| `cpu.efficiency_cores` | list of `{ opacity }`，最多 16 项 |

核心列表中的 `opacity` 已映射到 0.18–1.0，可直接用于 SVG 的 `fill-opacity`。

### 内存

| 表达式 | 类型 |
|---|---|
| `memory.used_gib` | float |
| `memory.total_gib` | float |
| `memory.used_percent` | float，0–100 |
| `memory.bar_width` | float，0–230 |

### NVIDIA GPU

GPU 和 NVML 不可用时，多数字段为 `none`。

| 表达式 | 类型 |
|---|---|
| `gpu.load_percent` | integer 或 `none` |
| `gpu.temperature_c` | integer 或 `none` |
| `gpu.power_w` | float 或 `none` |
| `gpu.power_limit_w` | float 或 `none` |
| `gpu.memory` | object 或 `none` |
| `gpu.memory.used_gib` | float |
| `gpu.memory.total_gib` | float |
| `gpu.memory.bar_width` | float，0–230 |
| `gpu.graphics_clock_mhz` | integer 或 `none` |
| `gpu.memory_clock_mhz` | integer 或 `none` |
| `gpu.performance_state` | integer 或 `none` |
| `gpu.fan_percent` | integer 或 `none` |

### I/O

以下字段都是 float 或 `none`，单位为 MiB/s：

- `io.network_down_mib_s`
- `io.network_up_mib_s`
- `io.disk_read_mib_s`
- `io.disk_write_mib_s`

## 常用语法

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

温度颜色可以直接在模板里表达：

```jinja
fill="{% if cpu.temperature_c is none %}#7f8790{% elif cpu.temperature_c >= 85 %}#d74242{% elif cpu.temperature_c >= 71 %}#e46d32{% elif cpu.temperature_c >= 56 %}#d29a26{% else %}#25a779{% endif %}"
```

## 16 段音频频谱

在模板中加入精确标记：

```xml
<g id="audio-spectrum"/>
```

该标记会启用 WASAPI 回环采集。为了避免每 200 ms 重新解析和栅格化整份 SVG，16 段频谱由 Rust 在最终合成阶段直接绘制；目前位置、方格大小和颜色定义在 `src/render.rs` 的 `AUDIO_*` 常量中。删除标记后，不会打开音频端点，也不会产生频谱刷新。

频谱表示默认播放端点的实时频率能量，而不是音量峰值。实现使用 48 kHz 等实际混音格式、1024 点 Hann 窗 FFT、16 个保证非空的对数频段，以及独立的上升/衰减平滑。

## 更新节奏

- 传感器按 `sensor_interval_ms` 采样；
- SVG 仪表层最多每秒重新栅格化一次；
- 频谱默认每 200 ms 更新并与缓存仪表层合成；
- GIF 保留素材帧间隔，同时受 `fps` 上限约束；
- 静态背景只在仪表或频谱变化时重绘，设备空闲时仍每秒重发缓存帧保活。

修改模板后，从托盘退出并重新双击 `LoopPanel.exe` 载入。提交前运行渲染单元测试，并在已授权的设备窗口中确认文字边界、透明区域和动态频谱。
