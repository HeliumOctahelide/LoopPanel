# 第三方声明

LoopPanel 使用以下直接 Rust 依赖；精确的传递依赖版本记录在源码包的 `Cargo.lock` 中。

| 组件 | 版本 | 许可 |
|---|---:|---|
| anyhow | 1.0.104 | MIT 或 Apache-2.0 |
| image | 0.25.10 | MIT 或 Apache-2.0 |
| libloading | 0.9.0 | ISC |
| minijinja | 2.21.0 | Apache-2.0 |
| resvg | 0.47.0 | MIT 或 Apache-2.0 |
| serde | 1.0.229 | MIT 或 Apache-2.0 |
| toml | 0.9.12+spec-1.1.0 | MIT 或 Apache-2.0 |
| windows-sys | 0.61.2 | MIT 或 Apache-2.0 |

可选 CPU 温度功能随程序嵌入 [PawnIO.Modules 0.2.10](https://github.com/namazso/PawnIO.Modules/tree/0.2.10) 的 `IntelMSR` 模块。该模块采用 LGPL-2.1-or-later；本源码包保留其 `IntelMSR.bin`、`IntelMSR.p` 与 `COPYING`，交付目录的 `licenses/PawnIO.Modules-0.2.10/` 还随附完整官方 tag 源码归档，其中包含 `include/pawnio.inc` 和构建任务。所用官方模块发布包为 [release_0_2_10.zip](https://github.com/namazso/PawnIO.Modules/releases/download/0.2.10/release_0_2_10.zip)，SHA-256：

```text
971c7c974c538b62ac020e0442fa99d0423417bfb496dfe9a4a43ccc0abc0e63
```

完整对应源码取自 [0.2.10 tag 归档](https://github.com/namazso/PawnIO.Modules/archive/refs/tags/0.2.10.zip)，交付文件名为 `PawnIO.Modules-0.2.10-source.zip`，SHA-256：

```text
afb96a3d6f562350d3cd0b0af1ca3dc5c3d53ff6dc4d28b15d23f015b2a4030d
```

PawnIO 2.x 驱动和 `PawnIOLib.dll` 是用户另行安装的可选运行依赖，不随 LoopPanel 二进制分发。

设备初始化时序、UYVY 公式和帧传输格式以 [Fakinvisibility/b360gt-driver](https://github.com/Fakinvisibility/b360gt-driver/tree/94ae7f2a710123b582ca3fa806d85ee1c684e287) 的 Windows USBPcap 实机捕获实现为基线。该项目采用 MIT License，Copyright (c) 2026 B360GT contributors。

本交付中的 `THIRD-PARTY-LICENSES.txt` 汇集了当前锁定依赖随源码发布的许可文件。
