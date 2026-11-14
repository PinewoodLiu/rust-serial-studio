# Rust Serial Studio · 串口助手

使用 Rust 编写的中文串口与网络调试工具，支持串口、TCP、UDP、HEX 收发、CRC 校验、多条指令和原始数据采集。

**当前版本：v0.1.4** · **Windows 10/11 x64** · **MIT License**

Designed by **liusonglin**, Powered by **Codex**

[下载最新版](https://github.com/PinewoodLiu/rust-serial-studio/releases/latest) · [使用指南](docs/USAGE.md) · [更新记录](CHANGELOG.md) · [验证记录](验证记录.md)

## 下载与运行

1. 打开 [Releases](https://github.com/PinewoodLiu/rust-serial-studio/releases)，下载 serial-assistant-0.1.4-windows-x64.zip。
2. 解压后运行 串口助手-0.1.4.exe，无需安装 Rust、Python 或浏览器运行环境。
3. 连接 USB 串口设备，选择端口、波特率与串口参数，再打开连接。

没有硬件时，可选择 **回环测试**：发出的字节会在本地原样返回，便于体验收发、校验、定时发送与文件传输。程序启动时不会自动连接设备或开始发送。

软件的串口功能可离线使用；USB 转串口设备仍需相应驱动。当前发行包面向 **Windows 10/11 64 位，不支持 Windows 7，也不是 32 位版本**。界面使用系统中文字体和 OpenGL 渲染，老旧显卡及远程桌面环境需自行验证。

## 功能

| 功能 | 支持内容 |
| --- | --- |
| 串口连接 | 端口枚举、自定义波特率、5～8 数据位、1/2 停止位、None/Odd/Even 校验、软/硬流控、DTR/RTS |
| 网络调试 | TCP 客户端、单客户端 TCP 服务端、固定远端 UDP |
| 文本与 HEX | UTF-8、GBK、ASCII；连续 HEX、空格分隔及 0x 前缀；转义符和 CR/LF/CRLF |
| 校验组包 | SUM8、XOR8、Modbus CRC16、CRC16/XMODEM、CRC32；可排除帧头与帧尾 |
| 多条指令 | 右侧紧凑表格、逐条 HEX、发送次数、延时、注释、箭头调整执行顺序；JSON / SSCOM INI 导入 |
| 定时发送 | 单条按次数发送、整组按轮数循环、进度显示与手动停止 |
| 文件收发 | 原始字节分块发送、可取消；RX 原始数据独立写入文件，保护已有接收文件 |
| 数据监视 | 时间戳、RX/TX、HEX 显示、筛选、暂停显示、日志导出、完整记录查看与复制 |
| 布局与统计 | 左右宽度和上下高度可拖动调整；统计始终为 B，速率为 B/s；参数和尺寸自动保存 |

## 发送次数与顺序

多条指令始终 **从上到下** 执行，使用箭头调整行位置。

| 设置 | 含义 | 0 的含义 |
| --- | --- | --- |
| 每条指令的“发送次数” | 当前指令连续发送 N 次后执行下一条 | 跳过该条 |
| 右侧顶部“循环轮数” | 勾选循环后，整组执行 N 轮后停止 | 不限轮数 |
| 底部单条“发送次数” | 单条定时发送 N 次后停止 | 不限次数 |

例如，A 的次数为 3、B 的次数为 2，整组循环 2 轮，发送顺序为 AAABBAAABB，共 10 次。每次发送后都会遵循当前指令的延时。

双击字符串或使用右键菜单可编辑注释；点击注释按钮按本条次数发送。底部的手动“发送”按钮始终只发送一次。

## 接收显示与长时间采集

串口和 TCP 的系统读取边界不等于消息边界。默认使用 **20 ms 接收空闲间隔** 合并相邻读取块，可在监视器中调整；这是一种显示规则，不是协议帧解析。UDP 保留数据报边界。

显示缓存约为 4194304 B / 3000 条，较早记录会自动移除。**长时间采集请使用“接收数据到文件”**，原始 RX 字节直接保存，不添加时间戳、TX 或分隔符，不受显示合并影响。导出日志仅包含当前缓存。

持续接收和高流量场景有显示大小、延时与队列上限；界面处理不及时会提示省略的显示块。设备/驱动缓冲溢出、慢速存储或 UDP 丢包仍可能导致采集不完整，详情见 [使用指南](docs/USAGE.md)。

## 从源码构建

验证环境：Windows x64、Rust 1.100.0、Visual Studio 2022 C++ 工具与 Windows SDK。构建脚本使用 PowerShell 7。

~~~powershell
git clone https://github.com/PinewoodLiu/rust-serial-studio.git
cd rust-serial-studio

cargo test --all-targets --locked
cargo clippy --all-targets --locked -- -D warnings
cargo run --locked

# 生成优化版、使用说明及许可证
.\build.ps1 -OutputName '串口助手-0.1.4.exe'
~~~

输出位于 dist/。首次构建需要下载 Cargo 依赖，运行已编译的软件无需联网。Cargo.lock 已提交，用于固定依赖版本。

~~~text
src/
  main.rs        桌面入口
  app.rs         中文界面、日志、交互
  layout.rs      分隔条与区域大小调整
  config.rs      配置保存、指令库与旧格式迁移
  protocol.rs    编码、转义、HEX、CRC 与组包
  receive.rs     接收显示合并与缓存边界
  transport.rs   后台通信、调度、文件发送与原始采集
docs/USAGE.md    详细操作指南
build.ps1       Windows 构建脚本
~~~

## 验证与限制

v0.1.4 的 **29 项自动化测试通过**，覆盖 CRC 标准向量、中文跨块解码、配置迁移、TCP/UDP、本地二进制回环、有限次数和逐条重复发送、文件逐字节比对及拖动交互。格式和 Clippy 严格检查通过，详见 [验证记录](验证记录.md)。

实际串口链路、USB 热拔插、DTR/RTS 电平和长时间高吞吐采集仍需使用目标设备验证。TCP 服务端每次只接受一个客户端，断开后需重新打开监听。

本项目参考 SSCOM 的常用调试流程独立实现，不包含原版可执行文件或源码。当前未实现 ISP 烧录、VT 终端仿真和波形显示，不宣称与 SSCOM 全功能兼容。

## 许可证与致谢

项目使用 [MIT License](LICENSE)。界面基于 [egui/eframe](https://github.com/emilk/egui)，串口通信使用 [serialport-rs](https://github.com/serialport/serialport-rs)，中文编码使用 encoding_rs。

第三方许可证见 [THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md)，同样包含在 Windows 发布包中。
