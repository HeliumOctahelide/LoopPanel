# 八个 PR 的整合修复与验证

日期：2026-11-11（香港时间）。分支：`codex/pr-integration-fixes`。

## 来源与范围

从 PR #8 的 `3bfd4ac9481fa1e38420bfccda64298d002bd1b8` 建立独立工作树，包含 #1 → #2 → #3 → #5 → #6 → #7 → #8 的原始提交；另外将 #4 的独立提交 `1f507b3f7f594b72a67f0c0c1233ce9f7b2f3ccd` 以保留来源说明的 cherry-pick 整合为 `2e52a0e`。

用户授权退出当前 LoopPanel、运行测试版本并通过摄像头检查实机。所有修复和测试辅助材料都在独立工作树或主仓库忽略的 `target` 目录中。主分支及原安装目录的配置未改动；未修改驱动、安装或替换温度服务、发表评论、推送或合并远端 PR。

## 修复

1. 配置注释使用已存在于依赖树中的 `toml_parser` 词法器，只替换独占一行的真实注释，保留多行字符串和行尾注释。
2. 自动翻译先将原配置复制到同目录临时文件，保留 Windows 文件安全属性，再写入、同步并以单次重命名替换；只读、部分写入失败和替换失败均保留原配置。
3. Windows GPU 占用率按物理适配器和实际引擎汇总进程，再取最大值，避免同类型不同引擎相加。
4. 配置重载立即重排采样期限，避免较短的新间隔继续等待旧期限。
5. 显式选择布局有独立请求标志，重新选择原先选中的布局也会跳回并重新计时。
6. 重载和布局切换先准备完整渲染内容与帧包，成功后一起替换。失败保留当前有效内容；轮换遇到坏模板仍可继续下一轮。
7. 单位切换只修改已接受的渲染器，失败恢复旧单位，不顺带读取磁盘改动或重置 GIF。

真实 GPU 查询测试标记为需显式运行的硬件测试。Rust 1.99 的 Clippy 要求将原有固定分块迭代改为等价的 `as_chunks::<8>()`；协议捕获字节测试仍通过。TM-280 文档表述收窄为贡献者的单台运行报告，完整设备验收材料仍待补充。

## 离线验证

工具链：Rust/Cargo 1.99.1，`x86_64-pc-windows-gnullvm`，含 rustfmt、clippy、rust-mingw。使用项目原有目标及 rust-lld 链接器，依赖按锁文件离线构建。

| 检查 | 结果 |
|---|---|
| `cargo fmt --all -- --check` | 通过 |
| `cargo test --all-targets --locked --offline` | 60 项通过；1 项硬件测试留待显式执行 |
| `cargo clippy --all-targets --locked --offline -- -D warnings` | 通过 |
| `cargo build --release --bins --locked --offline` | 通过 |
| `git diff --check`（修复增量） | 通过 |

新增八项回归测试覆盖多行字符串、部分写入失败、替换失败与只读文件、同类型不同 GPU 引擎、重复选择布局、重载采样调度、重载/轮换失败保持有效内容、单位切换保持已接受资源。

修改前的两个纯离线反例已确认失败：GPU 聚合返回 100 而预期为 40；注释翻译将 `title` 的多行字符串内容改写。修复后对应反例通过。重载原本与 USB 主循环耦合，现将内容准备与提交提取为 `DisplayContent`，用于实际运行与离线测试。

## 实机验证

设备为用户现有 Intel CPU + NVIDIA GPU + TM-360 环境。通过用户已经打开的 Aoni HD Camera 页面观察，页面报告 640×480、约 17 FPS。

首先运行独立目录中的测试版 `LoopPanel.exe`，确认原 GIF 背景、CPU/GPU 温度、负载及音频频谱正常。随后正常退出托盘实例，使用临时测试控制器直接调用同一库的 `display::run_until` 和 `DisplaySettings`，持有真实全局显示锁，逐项驱动布局与重载。控制器仅位于忽略的测试材料目录，不属于发布入口。

| 场景 | 实测结果 |
|---|---|
| 默认、简洁、CPU + GPU、双温度、时钟 | 均显示正常 |
| `show_sensors=false`、`show_clock=false` 下的大字布局 | 持续显示变化的读数 |
| 摄氏/华氏、三位数温度、关闭背景 | 正常，画面可读 |
| 自动轮换后重新选择原先的简洁布局 | 立即跳回 |
| 成功重载字体、亮度、模板和采样配置 | 显示 RGB 色条及 `RELOAD OK` 测试卡，时钟继续更新 |
| 错误 TOML 重载 | 拒绝，旧测试卡时钟继续更新 |
| 坏 Jinja 模板重载后完整轮换 | 拒绝坏模板；轮到默认页再次拒绝；之后继续切到其他布局，显示线程未退出 |
| 磁盘模板损坏时切换温度单位 | 已接受画面成功从 F 切换到 C |
| 正常停止测试控制器 | `DISPLAY_EXIT result=Ok(()) rejected=4` |
| 恢复原版 LoopPanel | 原画面恢复，原配置逐字节保持不变，测试前显示选项已恢复 |

单独执行 `windows_gpu::tests::queries_the_local_adapter_without_inconsistent_results --ignored --nocapture` 通过。本机实际返回 GPU 占用率约 4%、温度 37°C、显存用量 2,972,549,120 字节、总量 16,753,098,752 字节；这是 NVIDIA 上的 Windows 后备路径验证。

读取最终 PE 资源目录确认：托盘程序具有类型 24、编号 1 的 manifest，含 `PerMonitorV2, PerMonitor` 和 `asInvoker`；温度服务没有该清单资源。

## 验证边界

- 初次普通权限直接打开新的 Intel PawnIO 读取模块返回 `0x80070005`。随后经用户授权的独立管理员只读测试已通过 10 次读取，见[温度与帧率补验](TEMPERATURE_AND_FPS_VALIDATION.md)。现有服务未被替换，因此仍不宣称新版服务安装和生命周期已完成实机回归。
- 本机没有 AMD CPU/Radeon 或 Intel GPU，相关路径仅有离线测试与既有作者报告，不能据此宣布这些硬件组合通过本次实测。
- 实机控制器验证了菜单选项背后的生产设置及显示路径，未完整自动化托盘鼠标/键盘交互和错误弹窗；DPI 验证限于清单资源，未覆盖混合缩放多显示器。
- 摄像头画面存在倾斜、反光和偶发曝光叠影，适合确认布局、色条、刷新与恢复，不用于精密色彩测量。

## 本地材料

主仓库 `target/pr-validation/` 保存最终检查日志、硬件控制器源码与日志、配置备份、清单检查结果及 `evidence/` 中的十三张摄像头截图。关键截图为 `09-reload-success.png`、`10-invalid-config-keeps-running.png`、`11-rotation-survives-bad-template.png`、`12-unit-switch-ignores-bad-disk-template.png`、`13-original-app-restored.png`。

最终复审：规范轴与功能轴发现的两处修复回归均已处理，复核未发现新的实质阻断项。
