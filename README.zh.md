# fx991-rs

**卡西欧 fx-991CN X**（VerF）科学计算器模拟器，用 Rust 写成，运行真实的固件。

[English](README.md) | 简体中文

[![CI](https://github.com/moehiroshiro77/fx991-rs/actions/workflows/ci.yml/badge.svg)](https://github.com/moehiroshiro77/fx991-rs/actions/workflows/ci.yml)

## 需要你自备的文件

**固件不随仓库分发。** 它是卡西欧的版权 ROM，你需要自己准备 fx-991CN X **VerF** 的 dump：

| 文件 | 内容 | |
|---|---|---|
| `data/rom_verF.bin` | ROM 镜像 | 262 144 字节，MD5 `47bbf88fb3a9432b311b423f9b766e8f` |
| `data/skin.rgba` | 面板贴图 | 307×615，8 位 RGBA，无文件头，755 220 字节 |
| `data/_disas_verF.txt` | 反汇编列表（可选） | 只有 `emu disas` 需要 |

三个路径都可以覆盖：`--rom PATH`、`--skin PATH`、`--listing PATH`。

面板贴图是裸像素缓冲，按行排列的 RGBA，没有文件头。用任意 PNG 库解码计算器的
`interface.png` 后把像素写出来即可；模拟器会校验长度，尺寸不对会直接报错，不会画出
一堆乱码。

**缺这些文件也能跑测试**：crate 内部的单元测试照常通过，集成测试会跳过，所以刚
clone 下来 `cargo test` 是全绿的。

## 构建与运行

```bash
cargo build --release

target/release/emu calc "1+2*3"           # 7
target/release/emu key "1+2" --enter      # 先看输入区，再看答案
target/release/emu render face.png --keys "1+2" --enter
target/release/emu --help

target/release/fx991cnx                   # 可点击的窗口
```

窗口需要显卡，命令行工具不需要。

## 许可

**GNU 通用公共许可证第 3 版**，见 [`LICENSE`](LICENSE)。工作区内 10 个 crate
全部是 `GPL-3.0-only`，没有例外。

在法律允许的范围内，本程序**不提供任何担保**。它是自由软件：你可以按 GPL 的条款
再分发和修改；如果你分发修改版，必须提供对应的源码。

提交贡献即表示你同意贡献内容按同一许可授权（GPL-3.0 第 5 条，不需要另外签署协议）。

**固件不在许可范围内**，也不随仓库分发。`data/` 写在 `.gitignore` 里，不会被误提交。

本项目与卡西欧无关联，也未获其认可。

## 致谢

指令集、标志位行为与寻址方式来自 **OKI nX-U8/100 核心指令手册**
（FEZ0317A0-U8-INST-02），其余行为通过与固件实测比对确定。手册表述不清的地方由实验
定论，源码注释会标明哪一处是实测得出的。

### [CasioEmuNeo](https://github.com/qiufuyu123/CasioEmuNeo) — GPL-3.0

这是最有参考价值的项目，也是本项目能做成的前提。它把外设地址映射、定时器与中断
模型、屏幕合成都记录得足够细，可以拿来核对实现。本项目的结构沿用了它：crate 划分、
handler 命名（`OP_PUSH` → `op_push`）、屏幕常量（`N_ROW`、`ROW_SIZE`、`OFFSET`）
都来自那里，这也是本仓库同为 GPL-3.0 的原因。

与它的差异及原因：

| | CasioEmuNeo | 本实现 | 原因 |
|---|---|---|---|
| 时钟 | 由墙钟驱动 | 每条指令 1 tick，另加实时节流 | 实验可复现；实时性交给前端补 |
| 定时器分频 | 由实时时钟驱动 | 纯指令计数器 | 行为相同，但确定 |
| CW-II / `TIMER_SKIPPED` 分支 | 有 | 省略 | 只在 `HW_CLASSWIZ_II` 编入，VerF 固件走不到 |
| `0x49800` 的 RAM 镜像 | 非硬件机型上有 | 不实现 | VerF 机型报告 `real_hardware = 1` |
| 协处理器 `CRn` | 有实现 | 抛 `Unimplemented` | 固件从不执行它；报错好过静默执行错 |
| 显示字体 | 从 `interface.png` 取精灵 | 逐位输出点阵 | 调试够用；界面用真实贴图合成 |

### 其他参考

均为 GPL-3.0，且没有为本仓库贡献代码——读它们是为了了解机器和指令集。

| | |
|---|---|
| [991CN-X-CW-Decompilation](https://github.com/Physics365/991CN-X-CW-Decompilation) | ROP 教程、汇编器和一个 nX-U8 语言模块。针对 VerC，地址不能直接搬到 VerF，但方法可以 |
| [ropide-vscode-plugin](https://github.com/Yaing-Yan/ropide-vscode-plugin) | ROP 工具，内置 VerF 预设表。作为起点有用，但其中几个地址是错的 |
| [fxesplus](https://github.com/qiufuyu123/fxesplus) | 反汇编器源码和一份 nX-U8 指令表 |
