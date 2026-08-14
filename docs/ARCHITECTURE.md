# 架构说明

LoopPanel 分为可复用的上层显示管线和当前 TM-360 专用的设备传输层。

```text
配置 + PNG/JPEG/GIF/内置动效
                │
                ├── 480×270 中央背景
                │
Windows/NVML ── MiniJinja ── SVG ── resvg ── 缓存 RGBA 仪表层
传感器          │
                └── WASAPI ── Hann 窗 ── FFT ── 16 段方格
                                      │
                              480×480 RGBA
                                      │
                         limited-range UYVY422
                                      │
                         MS9132 帧头 + 帧尾
                                      │
                    HID 控制 + libusb0 EP04 bulk OUT
```

## 模块

| 文件 | 职责 |
|---|---|
| `config.rs` | TOML 读取、相对路径解析和取值约束 |
| `media.rs` | PNG/JPEG/GIF、内置动效与中央 16:9 背景 |
| `render.rs` | MiniJinja 上下文、SVG 栅格化、Alpha 合成和频谱绘制 |
| `audio.rs` | WASAPI 回环、混音格式解码、FFT、16 段和平滑 |
| `monitor.rs` | CPU 拓扑/负载/频率、内存、NVML 与 I/O 快照 |
| `io_metrics.rs` | IP Helper 网络计数与 PDH 磁盘速率 |
| `temperature*.rs` | PawnIO Intel Package 温度客户端和最小服务 |
| `protocol.rs` | 480×480 RGBA → UYVY422 与帧封装 |
| `transport.rs` | TM-360 枚举、HID 初始化、bulk 分块与帧发送 |
| `display.rs` | 传感器、仪表、频谱、动画和保活调度 |
| `tray.rs` | 单一 GUI 入口、首次 UAC 温度服务安装、Win32 托盘、工作线程和显式退出 |
| `startup.rs` | HKCU 登录启动 |
| `process.rs` | JONSBO-AIO 冲突检查和全局显示锁 |

## 权限边界

托盘、模板、媒体、监控和 USB 显示进程都以普通用户运行。Windows 不提供适用于当前 Intel CPU 的普通用户 Package 温度接口，因此温度功能单独使用一个 LocalSystem 服务。首次双击主程序且服务缺失时，主程序通过 `ShellExecuteExW` 请求 UAC、等待安装完成并验证普通用户管道读取；后续启动不再提权。服务只暴露固定的一字节采样请求和温度响应，不允许客户端指定 MSR 或执行写操作。

## 调度

- 传感器默认 1 Hz；
- SVG 仪表层最多 1 Hz；
- 音频频谱 5 Hz；
- 背景按 GIF 原始延迟或配置的 FPS 上限；
- 内容没有变化时每秒重发缓存帧，这是当前 TM-360 实机验证过的保活条件。

调度只在内容实际变化时进行 RGBA 合成、UYVY 转换和 USB 发送，避免静态背景按配置 FPS 空转。

## 动态库

程序动态加载 Windows 系统目录中的 `hid.dll`、`setupapi.dll`、`libusb0.dll`、`nvml.dll`、`iphlpapi.dll` 和 `pdh.dll`。缺少 NVML 或部分监控能力只会使相应模板字段成为 `none`；缺少显示所需的 HID/libusb0 则写屏失败并返回明确错误。
