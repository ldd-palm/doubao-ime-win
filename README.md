# Doubao Voice Input (豆包语音输入)

Windows 语音输入工具，基于豆包 ASR 实现实时语音识别。

## 功能特性

- 🎤 **实时语音识别** - 基于豆包 ASR 的高精度语音识别
- ⌨️ **可配置热键触发** - 双击 / 单击 / 组合键三种模式任选，支持区分左右 Ctrl/Shift/Alt；
  支持"按住触发键 + 第二个键"的组合来暂停/恢复整个服务
- 📍 **悬浮按钮** - 现代风格可拖动悬浮按钮，左键切换录音，右键退出
- 🔄 **流式识别** - 实时显示识别结果，支持文本修正
- 🔌 **断线自动重连** - ASR 会话中途断开会自动重连（带退避重试），悬浮按钮状态跟真实录音状态同步
- 🤖 **大模型纠错（可选）** - 接入任意 OpenAI 兼容 API（默认 DeepSeek）修正同音字/错别字/标点
- 🖥️ **系统托盘** - 英文菜单，左键/菜单均可暂停整个服务，Help 菜单带项目主页链接
- 📦 **绿色便携** - 单文件可执行，静态链接不依赖 VC++ 运行库，无需安装，自带图标

## 快速开始

### 下载使用

1. 从 [Releases](https://github.com/ldd-palm/doubao-ime-win/releases) 下载最新版本（只有一个
   exe 文件，静态链接，不需要额外装东西）
2. 放到任意目录，双击运行 `doubao-voice-input.exe`
3. 首次运行会自动：在同目录生成带完整注释的 `config.toml`、注册设备生成 `credentials.json`
4. 想用大模型纠错功能：运行一次生成 `config.toml` 后，手动编辑同目录下的 `config.toml`，
   在 `[llm]` 段填好 `model`/`api_key`/`base_url`（详见下方「大模型纠错」一节），默认不带任何
   key，也默认关闭

### 使用方法

1. **快捷键**（默认组合键 `Ctrl+Shift+V`，`config.toml` 的 `[hotkey]` 段可改）：
   热键有三种互斥的触发模式，`mode` 字段选一个：

   | mode | 行为 | 触发键 |
   |---|---|---|
   | `"combo"`（**默认**） | 按一下组合键，切换开始/停止 | `combo_key`，默认 `Ctrl+Shift+V` |
   | `"single_tap"` | 单击一下触发键，切换开始/停止 | `double_tap_key`，如 `RAlt` |
   | `"double_tap"` | 双击触发键（间隔内），切换开始/停止 | `double_tap_key` + `double_tap_interval` |

   `single_tap`/`double_tap` 两种模式下，`double_tap_key` 支持精确到左右：
   `Ctrl`/`LCtrl`/`RCtrl`、`Shift`/`LShift`/`RShift`、`Alt`/`LAlt`/`RAlt`
   （不带 `L`/`R` 前缀表示左右键都触发）。这两种模式还额外支持一个「暂停/恢复
   整个服务」的组合键：**按住 `double_tap_key` 不放、同时按一下 `pause_combo_key`**
   （默认 `Space`）—— 效果和下面第 3 条托盘左键点击一样：停止当前录音、热键的
   单击/双击失效、悬浮按钮隐藏；再按一次这个组合恢复。这个组合键本身在暂停期间
   **始终有效**（不然就没法用热键恢复了），留空 `pause_combo_key = ""` 可以关掉
   这个功能。（`combo` 模式不支持这个暂停组合键。）

   > 用修饰键（Ctrl/Shift/Alt）当 `single_tap`/`double_tap` 的触发键时，程序会
   > 在系统层面拦截这个按键，防止 Windows 把单独按 Alt 识别成"激活菜单"从而
   > 抢走输入框焦点。副作用见下方「已知限制」。

   **推荐配置示例**——单键触发语音输入、同一个键加空格触发暂停/恢复服务，
   不用组合键也不用双击：

   ```toml
   [hotkey]
   mode = "single_tap"
   double_tap_key = "RAlt"
   pause_combo_key = "Space"
   ```

   效果：
   - **单击右 Alt（`Right Alt`）** = 开始/停止语音输入
   - **按住右 Alt 不放、同时按一下空格（`RAlt + Space`）** = 暂停/恢复整个服务
     （悬浮按钮隐藏 = 已暂停，重新出现 = 已恢复；暂停期间单击右 Alt 不再触发
     语音输入，但 `RAlt + Space` 这个组合本身随时能用，用来恢复）

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
   - **右键点击** 打开菜单：`Start Voice Input` / `Stop Voice Input` / `Service: Pause`(或 `Service: Resume`) / `Help`(当前热键配置说明，按 `config.toml` 实际内容动态生成，带项目主页可点击链接) / `Exit`

## 配置文件

配置文件 `config.toml` 与程序同目录：

```toml
[general]
auto_start = false
language = "zh-CN"

[hotkey]
# "combo" (组合键，默认) / "double_tap" (双击) / "single_tap" (单击)
mode = "combo"
combo_key = "Ctrl+Shift+V"
# double_tap / single_tap 模式下生效，支持 Ctrl/LCtrl/RCtrl、
# Shift/LShift/RShift、Alt/LAlt/RAlt（不带 L/R 前缀表示左右键都触发）
double_tap_key = "Ctrl"
double_tap_interval = 300  # 毫秒，仅 double_tap 模式生效
# 按住触发键同时按这个键 = 暂停/恢复整个服务；留空禁用。
# 仅 double_tap / single_tap 模式生效，combo 模式不支持。
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
# 克隆项目（这个 fork；上游原版见文末「致谢」）
git clone https://github.com/ldd-palm/doubao-ime-win.git
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

项目已配置 GitHub Actions 自动构建（`.github/workflows/build.yml`）：
- 推送到 `main`/`master` 分支时自动构建（仅验证，不发布）
- 创建 `v*` 标签时自动发布 Release——**只打包 exe 一个文件**，不带 `config.toml`
  （首次运行会自动生成带完整注释的默认配置，不会带着发布者的个人设置或密钥）

### 部署到其他机器

**只需要 exe 一个文件**，[Releases](https://github.com/ldd-palm/doubao-ime-win/releases)
下载的就是干净的、不带任何 key 的版本，双击就能跑，首次运行自动生成
`config.toml`（默认关闭大模型纠错，其他都是安全默认值）。

- **不要**拷贝 `credentials.json`——那是本机专属的设备身份，新机器首次运行会
  自动重新注册一个新的，不需要手动处理。
- 想省去新机器上重新配置热键/大模型纠错的麻烦：把你自己那台机器上、exe 同目录的
  `config.toml` 一起拷过去即可（程序按 exe 所在路径读配置，必须放同一目录）。
  如果这份 `config.toml` 的 `[llm] api_key` 留空（用的是 `LLM_API_KEY` 环境变量），
  新机器上要么重新设一遍这个环境变量，要么把 key 直接填进拷过去的
  `config.toml`（会变成明文，仅在确定这份文件不会再转发给别人时这么做）。

## 已知限制

- **启动有感知延迟**：按热键/点悬浮按钮到真正开始识别，中间有一次完整的 ASR
  WebSocket 握手（TCP+TLS+两次协议往返），实测约 1.5-2.5 秒，其中 TLS 握手本身
  占大头。已经做了"立即切到处理中状态"的即时反馈，但延迟本身还在——要彻底消除
  需要常驻一个预热连接，目前未实现（协议是逆向的，不确定服务端允许空闲连接
  多久，贸然做容易引入新问题）。
- **修饰键当热键会导致对应的系统功能失效**：`single_tap`/`double_tap` 模式下，触发键
  会在系统层面被整个吃掉（防止 Windows 把单独按 Alt 识别成"激活菜单"抢走输入框
  焦点），如果用 `RAlt` 当热键、键盘布局又依赖右 Alt 当 AltGr 输入特殊字符，
  程序运行期间会失效；左 Alt / Alt+Tab / Alt+F4 不受影响。
- **ASR 断线重连有次数上限**：最多自动重试 3 次（1s/2s/4s 退避），服务端持续
  不可用的话最终还是会停止录音，需要手动再触发一次。
- **LLM 纠错依赖外部服务可用性**：所用的大模型 API 抖动或过期会导致纠错静默跳过
  （回退用原始识别结果），不影响主流程，但纠错效果会跟着服务状态波动。

## 技术架构

| 模块 | 技术 |
|------|------|
| 语言 | Rust |
| 语音识别 | 豆包 ASR (doubaoime-asr 协议) |
| 音频采集 | cpal |
| 音频编码 | Opus |
| 热键监听 | global-hotkey + Win32 底层键盘钩子 (WH_KEYBOARD_LL，双击/单击/组合键检测) |
| 大模型纠错 | 任意 OpenAI 兼容 Chat Completions API（默认 DeepSeek，可选） |
| 系统托盘 | tray-icon / muda，Help 对话框用 TaskDialogIndirect（带可点击链接） |
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

- [EvanDbg/doubao-ime-win](https://github.com/EvanDbg/doubao-ime-win) - 本项目 fork 自这个上游仓库（v1.1.1）
- [doubaoime-asr](https://github.com/starccy/doubaoime-asr) - 豆包 ASR 协议参考实现
