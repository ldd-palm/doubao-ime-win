# 交接文档：豆包语音输入 Windows 客户端

## 0. 当前状态（先看这里）

这份文档最初只为一个任务写的：「修复凭据过期导致语音输入失效」（见第 1-4 节）。
写的时候明确说"本机无法编译，只做过静态审查，未跑过 `cargo build`"。

**那个状态已经过去了。** 同一台机器后来装好了 Rust + protoc 工具链，第 1-4 节的
改动已经**编译通过并实测验证**；之后又在同一个会话里做了一整轮功能扩展和体验
优化（第 5 节），目前整体处于**初步可用状态**：能编译、能真实注册设备/连接
ASR、能识别语音、能用大模型纠错、能通过热键/悬浮按钮/托盘三种方式启停。

- 本机工具链版本：Rust 1.98.1（`winget install Rustlang.Rustup`）、protoc 36.0
  （`winget install Google.Protobuf`）、VS 2026 Community 自带的 MSVC 工具链、
  CMake 4.3.3（**需要**设置环境变量 `CMAKE_POLICY_VERSION_MINIMUM=3.5`，否则
  编译 `opus` crate 时 vendored libopus 的老版本 `cmake_minimum_required` 会被
  新版 CMake 拒绝）。
- `doubao-credential-fix.patch` **只覆盖第 1-4 节**这一轮改动。第 5 节的改动
  没有生成对应的 patch（这个目录不是 git 仓库，没法 `git diff`）——要看第 5 节
  具体改了哪些文件/函数，去看 `CHANGES.md` 的「第二轮改动」部分，那里是逐文件
  记录的。
- 运行时实际生效的配置是 `config.toml`（和 `target/release/config.toml` 保持
  同步的那份），当前设置：热键 `single_tap` + `RAlt`，LLM 纠错 `enabled=true`
  且已填真实 `endpoint_id`（API Key 走环境变量 `ARK_API_KEY`，没写在文件里）。

---

## 第一部分：凭据过期修复（第 1-4 节，已验证）

## 1. 问题现象

用户在学校发的笔记本上运行 `doubao-voice-input.exe`：双击 Ctrl 后悬浮按钮变红（进入录音态），
但**迟迟不出字或完全不出字**。控制台刷屏 `[AudioCapture] Channel full, dropping frame`。

## 2. 已完成的诊断（结论可信，有实测证据）

在原始 v1.1.1 二进制上做过对照实验，结论是**凭据过期**，与网络封锁、杀软、麦克风权限均无关：

| 检查项 | 结果 |
|---|---|
| DNS / TLS：`log.snssdk.com`、`is.snssdk.com`、`frontier-audio-ime-ws.doubao.com` | ✅ 全通。证书是原厂 DigiCert，**没有** Sophos 中间人拦截 |
| 系统代理 | ✅ 无（`ProxyEnable = 0`） |
| 麦克风权限 `ConsentStore\microphone\NonPackaged` | ✅ 三个 exe 路径均为 Allow |
| 麦克风采集 + Opus 编码 | ✅ 正常，能识别出中文 |
| 杀软 | Sophos Intercept X 在运行，但**未拦截**本程序（无相关事件日志） |
| **用新注册凭据**跑 CLI (`--cli`) 全链路 | ✅ 连接 2s → TaskStarted → SessionStarted → 实时出字 |
| **用旧凭据**（9/13 注册的）跑同一个 exe | ❌ 握手 19s、StartTask 16s，然后 `service discovery failure` |

关键日志对比（同一个 exe，仅 `credentials.json` 不同）：

```
# 旧凭据：
Connecting to ASR WebSocket ... (16:59:47)
WebSocket connected successfully  (17:00:05)   <- 拖了 19 秒
Sending StartTask
TaskStarted received             (17:00:22)   <- 又拖了 16 秒
ERROR ASR error: read backend response: rpc error: code = 2 desc = service discovery failure

# 新凭据：
Connecting to ASR WebSocket ... (17:00:36)
WebSocket connected successfully  (17:00:38)   <- 2 秒
TaskStarted received / SessionStarted received  <- 正常，随后出字
```

### 根因

`src/data/credential.rs` 原本只判断字段非空，**从不检查新鲜度**：

```rust
if creds.is_complete() { return Ok(creds.clone()); }  // 只看 device_id / token 非空
```

豆包 ASR token 是短效的。过期后服务端**不干脆拒绝**，而是把握手拖十几秒再以
`service discovery failure` 结束会话。同时因为原代码**先开麦、后连 ASR**，
握手期间采集的音频塞满容量 100 的 channel，于是刷屏丢帧（实测积压 1363 帧 / 27 秒）。

## 3. 本次改动（commit `c7f3210`，分支 `fix/credential-refresh`）

三层防御 + 一个时序修正。

### 3.1 主动过期判定
- `src/asr/device.rs`：`DeviceCredentials` 增加 `issued_at_ms: Option<u64>`（`#[serde(default)]`），
  `get_asr_token()` 成功后调 `mark_issued_now()` 打时间戳。
  新增 `age()` 与 `is_usable(ttl_days)`。
- `src/data/credential.rs`：`ensure_credentials()` 改用 `is_usable()`，按
  `asr.credential_ttl_days`（默认 3 天）判断。
- **旧凭据没有 `issued_at_ms` → 一律视为过期** → 用户现有的失效凭据在新版首次启动时自动重注册，
  无需手动删文件。这是刻意设计的迁移路径。

### 3.2 超时兜底 + 失败分类
- `src/asr/client.rs`：握手、`StartTask`、`StartSession` 三处都加
  `asr.connect_timeout_secs`（默认 8 秒）超时。
- 新增 `AsrSetupError { kind: SetupFailure, detail }`，`SetupFailure` 分
  `Timeout` / `StaleCredentials` / `Rejected`。
- `is_credential_failure()` 按关键字匹配（`service discovery failure`、`auth`、`token`、
  `unauthorized`、`permission`）判定是否疑似凭据问题。
- `AsrClient::with_timeout()` 为新增构造器；原 `new()` 保留（默认 8 秒），不破坏兼容。

### 3.3 会话中途失效的补救（**容易被忽略的一层**）
重看日志发现：`service discovery failure` **不是在 setup 阶段报的**，而是会话「建成」后
经 result channel 传回来的。所以单靠 3.2 拦不住它。因此在
`src/business/voice_controller.rs` 的 `ResponseType::Error` 分支：
若 `is_credential_failure()` 命中 → `store.invalidate()`（清内存 + **删除 credentials.json**）
→ 置 `credentials_dirty` 标记 → 下次 `connect_with_retry()` 开头先重注册。

### 3.4 时序修正：先连 ASR，再开麦
- `src/audio/capture.rs`：新增 `AudioCapture::channel()` 与 `start_into(sender)`；
  原 `start()` 保留为二者的包装，**签名不变**。
- `src/business/voice_controller.rs::start()`：先 `connect_with_retry()` 建好会话，
  成功后才 `start_into()` 开始采集。消除握手期间丢帧。
- 失败路径会把 `is_recording` 复位，避免控制器卡在录音态（原代码有这个隐患）。

### 3.5 自动重试
`connect_with_retry()`：首次失败且 `warrants_credential_refresh()`（`Timeout` 或
`StaleCredentials`）为真 → `force_refresh()` 重新注册 → 换掉共享的 `AsrClient` → 重试一次。
只重试一次，避免死循环。`Rejected`（其他原因）不重试，直接上报。

### 3.6 新增配置项
`config.toml` / `config.toml.example` 的 `[asr]` 段：

```toml
credential_ttl_days = 3     # 凭据最长缓存天数，0 = 不检查过期
connect_timeout_secs = 8    # ASR WebSocket 连接超时（秒）
```

两项都有 `#[serde(default)]`，**旧配置文件不会因缺字段而解析失败**。

## 4. 改动文件清单（第一轮）

```
README.md                        +21    新增「凭据过期与自动重注册」说明
config.toml                      +6     新增两个配置项
config.toml.example              +6     同上
src/asr/client.rs               +194/-   超时、AsrSetupError、with_timeout
src/asr/device.rs                +45    issued_at_ms、age、is_usable、mark_issued_now
src/asr/mod.rs                   +2/-2  导出 AsrSetupError / SetupFailure / is_credential_failure
src/audio/capture.rs             +19    channel() / start_into()
src/business/voice_controller.rs +163   connect_with_retry、脏标记、错误分支处理、时序调整
src/data/config.rs               +24    AsrConfig 两个新字段 + 默认值函数
src/data/credential.rs          +120    TTL 判定、force_refresh、invalidate
src/main.rs                      +37    Arc<CredentialStore>、with_credential_refresh 接线
```

---

## 第二部分：功能扩展与体验优化（第 5 节，本次会话，无 patch）

在第一轮的基础上，本机装好工具链后实际跑通了编译 + `--cli` 全流程 + 真实设备
注册/ASR 连接测试，随后按用户需求陆续加了这些东西。**详细的逐文件/逐函数改动
在 `CHANGES.md` 的「第二轮改动」部分**，这里只列结论和现状。

### 5.1 静态链接（`.cargo/config.toml`，新增文件）

`rustflags = ["-C", "target-feature=+crt-static"]`。产物不再依赖
`VCRUNTIME140.dll` 等，`dumpbin /dependents` 验证过只剩 Windows 系统自带 DLL，
真正做到拷贝即用。

### 5.2 热键：新增单击模式 + 区分左右键 + 修了一个焦点 bug

- `config.toml` 的 `[hotkey]` 新增 `mode = "single_tap"`（单击触发，不用双击），
  `double_tap_key` 支持 `LCtrl/RCtrl/LShift/RShift/LAlt/RAlt` 精确到左右。
- **修了一个真实体验问题**：用右 Alt 当热键时，点了输入框再按右 Alt 会导致
  输入框失焦，得重新点一次。根因是 Windows 把单独按 Alt 识别成"激活菜单"的
  系统手势，之前的底层键盘钩子只旁听不拦截，按键照常传给了前台窗口。现在
  钩子命中触发键时会直接吞掉这个按键（`WH_KEYBOARD_LL` 标准做法：返回非零值
  而不调用 `CallNextHookEx`），不再传播到系统/应用。
  副作用：右 Alt 被整个系统层面吃掉了，如果键盘布局靠右 Alt 当 AltGr 打特殊
  字符，程序运行期间会失效；左 Alt/Alt+Tab/Alt+F4 不受影响。

### 5.3 系统托盘：暂停/恢复整个服务 + 英文菜单

- 左键点击托盘图标，或菜单里的 `Service: Pause`/`Service: Resume`，效果一样：
  停止当前录音（如果在录）→ 热键失效 → 悬浮按钮隐藏 → 托盘图标变灰、tooltip
  变化。再点一次恢复。
- 菜单改成全英文：`Start Voice Input` / `Stop Voice Input` / `Service: Pause`
  / `Settings...` / `Exit`。
- 实现上有个坑记录一下：`tray-icon`/`muda` 的 `TrayIcon`/`MenuItem` 内部是
  `Rc<RefCell<..>>`，**不是 `Send`**。改状态的判断逻辑可以放在别的线程，但
  真正调用 `set_icon()`/`set_text()` 必须回到创建它们的那个线程（本项目是
  运行 Win32 消息循环的主线程），用一个 `tray_dirty` 标志位 + 不绑窗口的
  `SetTimer(None, 0, 150, None)` 做跨线程通知+定期刷新。

### 5.4 大模型纠错（火山方舟 / 豆包大模型）

- 新增 `src/business/text_corrector.rs`，只在 ASR **最终确认结果**上跑一次
  纠错（不碰实时候选字），失败/超时静默回退原文，不阻塞不丢字。
- `config.toml` 新增 `[llm]` 段，默认 `enabled = false`。API Key 优先读环境
  变量 `ARK_API_KEY`，其次读 `config.api_key`。
- **已用真实 key/endpoint 实测**：故意在文本里造"石物"→纠正为"食物"、
  "酸略"→纠正为"策略"，效果符合预期。

### 5.5 启动延迟：诊断 + 即时反馈（延迟本身未解决）

- `src/asr/client.rs` / `voice_controller.rs` 加了计时日志，实测定位到：从
  按热键到真正开始识别，约 **2.5 秒**是纯网络等待，其中 WebSocket 握手
  （TCP+TLS）占了约 2 秒，`TaskStarted`/`SessionStarted` 两次协议往返各
  ~200-300ms。
- 已经做的：三个启动入口（热键回调、托盘菜单 Start、悬浮按钮点击）现在会在
  发起连接的瞬间就把按钮切到"处理中"状态，不用等整个握手完才有反应。
- **没做的**：要把 2.5 秒基本降到 0，需要常驻一个预热的 ASR 连接，用户按键时
  直接复用。这个还没做——协议是逆向的，不知道服务端允许连接空闲多久，贸然
  做容易引入新的连接失效问题，需要先和用户确认这个取舍再动手。

---

## 6. 已知限制 / 待办（合并更新）

延续第一轮遗留项 + 第二轮新发现的：

- `is_credential_failure()` 仍是关键字启发式（第一轮遗留），协议换文案就可能失效。
  更稳的做法是看响应里的错误码字段（`src/asr/protocol.rs` 的 `parse_response`），未做。
- `credential_ttl_days` 默认 3 天仍是保守猜测（第一轮遗留），真实有效期未知。
- 会话中途失效只清凭据、不自动重连当次录音（第一轮遗留），用户需再按一次热键。
- **启动延迟 ~2.5s**（第二轮新发现，见 5.5），连接预热方案待用户拍板。
- **RAlt 吞键导致 AltGr 失效**（第二轮新发现，见 5.2），依赖右 Alt 打特殊字符的键盘布局会受影响。
- **LLM 纠错依赖火山方舟服务可用性**（第二轮新增功能的固有限制），服务抖动时纠错效果会跟着波动，但不影响主流程。
- `force_refresh()` 每次都走完整注册流程（第一轮遗留），理论上可只刷 token，当前选了简单可靠的路子，未优化。

## 7. 部署到其他机器

只需要两个文件（跟 exe 同目录）：

```
doubao-voice-input.exe   （或改名后的版本，内容不变；已静态链接，无需装 VC++ Redistributable）
config.toml                程序按 exe 所在路径读配置，必须同目录
```

- **不要**拷贝 `credentials.json`——设备身份专属这台机器，新机器首次运行会自动重新注册。
- 如果 `[llm] api_key` 留空（用的是 `ARK_API_KEY` 环境变量），新机器要么也设一遍这个环境变量，
  要么把 key 直接填进拷过去的 `config.toml`（会变成明文，转发给别人前要注意）。

## 8. 隐私提示

本压缩包**不含任何凭据**：`credentials.json` 已被 `.gitignore` 忽略，打包时也显式排除。
测试时生成的 `credentials.json` 含真实 device_id / token，**不要回传或提交**。

同理，`ARK_API_KEY` 环境变量、`config.toml` 里如果填了明文 `api_key`，也不要回传或提交。

---

原始仓库：https://github.com/EvanDbg/doubao-ime-win （上游 v1.1.1，commit `1c7a790`）
第一轮改动：commit `c7f3210`，分支 `fix/credential-refresh`（有 patch）
第二轮改动：本次会话，无 git 历史（目录不是仓库），详见 `CHANGES.md`
