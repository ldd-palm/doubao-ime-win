# Doubao Voice Input (豆包语音输入)

Windows 语音输入工具，基于豆包 ASR 实现实时语音识别。

## 功能特性

- 🎤 **实时语音识别** - 基于豆包 ASR 的高精度语音识别
- ⌨️ **可配置热键触发** - 双击 / 单击 / 组合键三种模式任选，支持区分左右 Ctrl/Shift/Alt
- 📍 **悬浮按钮** - 现代风格可拖动悬浮按钮，左键切换录音，右键退出
- 🔄 **流式识别** - 实时显示识别结果，支持文本修正
- 🤖 **大模型纠错（可选）** - 用豆包大模型（火山方舟）修正同音字/错别字/标点
- 🖥️ **系统托盘** - 英文菜单，左键/菜单均可暂停整个服务
- 📦 **绿色便携** - 单文件可执行，静态链接不依赖 VC++ 运行库，无需安装

## 快速开始

### 下载使用

1. 从 [Releases](https://github.com/EvanDbg/doubao-ime-win/releases) 下载最新版本
2. 解压到任意目录
3. 运行 `doubao-voice-input.exe`
4. 首次运行会自动注册设备

### 使用方法

1. **快捷键** (默认双击 Ctrl，可在 `config.toml` 改成组合键或单击触发):
   - 快速双击 `Ctrl` 键开始语音输入
   - 再次双击停止录音，文本自动插入到当前焦点窗口
   - 想改成单击某个键触发（比如右 Alt），见下方配置文件说明的 `single_tap` 模式

2. **悬浮按钮**:
   - 🟣 紫色 = 待机状态
   - 🔴 红色 = 正在录音
   - 🟠 橙色 = 处理中
   - **左键点击** = 开始/停止录音
   - **右键点击** = 退出程序（有确认提示）
   - **拖动** = 调整位置
   - 服务被暂停时（见下）悬浮按钮会隐藏

3. **系统托盘**（菜单为英文）:
   - **左键点击托盘图标** = 暂停/恢复整个服务——暂停后热键和悬浮按钮都失效，悬浮按钮隐藏，托盘图标变灰
   - **右键点击** 打开菜单：`Start Voice Input` / `Stop Voice Input` / `Service: Pause`(或 `Service: Resume`) / `Help`(当前热键配置说明，按 config.toml 实际内容生成) / `Exit`

4. **热键暂停/恢复服务**（`double_tap`/`single_tap` 模式下生效）:
   - **按住触发键的同时按下 `pause_combo_key`**（默认 `Space`，即按住 RAlt 同时按空格）= 暂停/恢复整个服务，效果和左键点托盘图标一样：悬浮按钮隐藏=已暂停，重新出现=已恢复
   - 暂停期间触发键的单击/双击不再切换录音，但这个组合键本身**始终有效**，用来恢复
   - 不想要这个功能：把 `config.toml` 里 `[hotkey] pause_combo_key` 设成空字符串 `""`

## 配置文件

配置文件 `config.toml` 与程序同目录：

```toml
[general]
auto_start = false
language = "zh-CN"

[hotkey]
# "combo" (组合键) / "double_tap" (双击) / "single_tap" (单击)
mode = "double_tap"
combo_key = "Ctrl+Shift+V"
# double_tap / single_tap 模式下生效，支持 Ctrl/LCtrl/RCtrl、
# Shift/LShift/RShift、Alt/LAlt/RAlt（不带 L/R 前缀表示左右键都触发）
double_tap_key = "Ctrl"
double_tap_interval = 300  # 毫秒，仅 double_tap 模式生效
# 按住触发键同时按这个键 = 暂停/恢复整个服务；留空禁用
pause_combo_key = "Space"

[floating_button]
enabled = true
position_x = 100
position_y = 100

[asr]
vad_enabled = true
# 设备凭据最长缓存天数（0 = 不检查过期）
credential_ttl_days = 3
# ASR WebSocket 连接超时（秒）
connect_timeout_secs = 8
```

### 凭据过期与自动重注册

豆包 ASR token 会在服务端过期。过期后服务端不会干脆拒绝，而是把 WebSocket
握手拖十几秒，再以 `service discovery failure` 结束会话 —— 表现为"按了热键、
图标变红，但迟迟不出字"，同时日志刷 `Channel full, dropping frame`。

程序对此做了三层处理：

1. **主动过期**：`credentials.json` 记录签发时间，超过 `credential_ttl_days`
   即视为失效并重新注册（旧版本写的、没有时间戳的凭据一律按过期处理）。
2. **超时兜底**：握手与会话建立受 `connect_timeout_secs` 约束，不再无限等待；
   超时或被拒时自动重新注册并重试一次。
3. **会话内失效**：若 `service discovery failure` 之类的错误在会话建立后才出现，
   会清除本地凭据，下次启动录音前先重新注册。

此外，录音改为"先连上 ASR、再开麦"，避免握手期间采集的音频被丢弃。

### 大模型纠错（可选）

豆包 ASR 的同音字/错别字有时需要人工修正。`[llm]` 配置段可以接入**任意
OpenAI 兼容的 Chat Completions API**（DeepSeek、火山方舟/豆包大模型等）对
**最终识别结果**做一次纠错，只在确认结果（非实时候选字）上生效，避免中间
结果被打断刷新。

```toml
[llm]
enabled = false            # 默认关闭，不影响原有行为
model = ""                 # 模型名，如 "deepseek-flash"，或火山方舟 ep-xxxxx 接入点 ID
api_key = ""                # 留空则读环境变量 LLM_API_KEY（推荐，不在文件里存明文 key）
base_url = "https://api.deepseek.com/chat/completions"  # 换成火山方舟等其他兼容接口也行
timeout_secs = 5            # 超时或调用失败直接用原始识别结果，不阻塞输入
```

当前实际用的是 **DeepSeek**（`deepseek-flash`）。换成火山方舟豆包大模型只需把
`base_url` 改成 `https://ark.cn-beijing.volces.com/api/v3/chat/completions`，
`model` 改成方舟控制台创建的 `ep-xxxxx` 接入点 ID，key 照样走 `LLM_API_KEY`。

纠错会给每句确认结果多引入一次网络往返（通常几百毫秒），属于用实时性换准确性；
调用失败/超时会静默回退到原始识别文本，不会卡住或丢字。

## 从源码构建

### 环境要求

- Rust 1.70+ (stable)
- Windows 10/11 x64
- Visual Studio Build Tools 2022
- CMake
- Protobuf Compiler (protoc)

### 构建步骤

```powershell
# 克隆项目
git clone https://github.com/EvanDbg/doubao-ime-win.git
cd doubao-ime-win

# 构建 Release 版本
cargo build --release

# 可执行文件位置
# target/release/doubao-voice-input.exe
```

`.cargo/config.toml` 里配了 `target-feature=+crt-static`，产物**静态链接 MSVC
运行库**，`dumpbin /dependents` 验证过不再依赖 `VCRUNTIME140.dll` 等，只剩
Windows 系统自带 DLL——拷到任何 Windows 10/11（哪怕从没装过 VC++ Redistributable）
都能直接跑。

> ⚠️ 如果本机 CMake 版本是 4.x，编译 `opus` crate 时可能报
> `Compatibility with CMake < 3.5 has been removed`（vendored libopus 的
> `cmake_minimum_required` 版本太老）。设置环境变量
> `CMAKE_POLICY_VERSION_MINIMUM=3.5` 即可绕过，建议设成永久的用户环境变量。

### GitHub Actions

项目已配置 GitHub Actions 自动构建：
- 推送到 `main` 分支时自动构建
- 创建 `v*` 标签时自动发布 Release

### 部署到其他机器

只需要两个文件，跟 exe 放同一目录：

```
doubao-voice-input.exe   （或改名后的版本，内容不变）
config.toml               程序按 exe 所在路径读配置，必须同目录
```

- **不要**拷贝 `credentials.json`——那是本机专属的设备身份，新机器首次运行会
  自动重新注册一个新的，不需要手动处理。
- 如果 `config.toml` 里 `[llm] api_key` 留空（用的是 `ARK_API_KEY` 环境变量），
  新机器上要么重新设一遍这个环境变量，要么把 key 直接填进拷过去的
  `config.toml`（会变成明文，仅在确定不会再转发给别人时这么做）。

## 已知限制

- **启动有感知延迟**：按热键/点悬浮按钮到真正开始识别，中间有一次完整的 ASR
  WebSocket 握手（TCP+TLS+两次协议往返），实测约 1.5-2.5 秒，其中 TLS 握手本身
  占大头。已经做了"立即切到处理中状态"的即时反馈，但延迟本身还在——要彻底消除
  需要常驻一个预热连接，目前未实现（协议是逆向的，不确定服务端允许空闲连接
  多久，贸然做容易引入新问题）。
- **`RAlt` 作为热键会导致 AltGr 失效**：`single_tap`/`double_tap` 模式下，触发键
  会在系统层面被整个吃掉（防止 Windows 把单独按 Alt 识别成"激活菜单"抢走输入框
  焦点），如果键盘布局依赖右 Alt 当 AltGr 输入特殊字符，程序运行期间会失效。
- **LLM 纠错依赖外部服务可用性**：方舟 API 抖动或过期会导致纠错静默跳过（回退
  用原始识别结果），不影响主流程，但纠错效果会跟着服务状态波动。

## 技术架构

| 模块 | 技术 |
|------|------|
| 语言 | Rust |
| 语音识别 | 豆包 ASR (doubaoime-asr 协议) |
| 音频采集 | cpal |
| 音频编码 | Opus |
| 热键监听 | global-hotkey + Win32 底层键盘钩子 (WH_KEYBOARD_LL，双击/单击检测) |
| 大模型纠错 | 豆包大模型 (火山方舟 Chat Completions API，可选) |
| 系统托盘 | tray-icon / muda |
| 悬浮按钮 | Win32 API (Layered Window) |
| 文本输入 | Windows SendInput API |

## 免责声明

> ⚠️ **注意**
> 
> 本项目基于豆包输入法客户端协议分析实现，非官方 API。
> - 仅供学习研究使用
> - 协议可能随时变更导致功能失效
> - 请遵守相关法律法规

## 许可证

MIT License

## 致谢

- [doubaoime-asr](https://github.com/starccy/doubaoime-asr) - 豆包 ASR 协议参考实现
