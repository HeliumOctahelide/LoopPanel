# Agent 工作说明

## 项目边界

LoopPanel 当前只对 README 中列出的 TM-360 硬件配置提供写屏支持。修改 `protocol.rs`、`transport.rs`、设备匹配、启动报告、分块或保活策略前，必须先阅读 `docs/DEVICE_PORTING.md`。

## 实机安全

- 未得到设备所有者明确授权时，只能运行单元测试、静态检查和离线构建；
- 不要为检查构建结果而启动 `LoopPanel.exe`；主程序会进入真实写屏路径，并可能请求安装温度服务；
- 不得自动安装驱动、USBPcap、服务或修改设备绑定；
- 不得发送未在现有捕获中出现的控制报告；
- 不得添加固件、EEPROM、描述符写入或 USB reset 路径；
- 写屏前确认 JONSBO-AIO 已退出，并使用全局显示锁。

## 实现约定

- 保持普通权限托盘与高权限温度服务的边界；
- 模板字段采用强类型上下文，缺失硬件值使用 `Option` / `none`；
- 新设备的纯数据差异优先使用配置结构，只有真实行为差异才增加枚举或 trait；
- 不要以 VID/PID 单独匹配设备，也不要加入自动试探初始化序列；
- 发布包只以 `LoopPanel.exe` 作为用户入口；不要重新引入启动脚本或面向用户的命令行入口。

## 必需检查

```powershell
cargo fmt --all -- --check
cargo test --all-targets --locked --offline
cargo clippy --all-targets --locked --offline -- -D warnings
cargo build --release --bins --locked --offline
```

若显示实例正在运行，单实例锁测试可能因真实全局锁而失败；不要为了让测试通过而停止用户进程或削弱锁。应先说明环境冲突，在获准的维护窗口重新运行完整测试。
