# 改动速查（逐文件 / 逐函数）

配套 `HANDOFF.md`。这里只列「改了什么」，原因见 HANDOFF 第 2、3 节。

上游基线：`1c7a790`（v1.1.1）。本次改动：`c7f3210`。
完整 diff 见 `doubao-credential-fix.patch`（可用 `git am` 或 `git apply` 应用到干净的上游仓库）。

---

## src/asr/device.rs

| 位置 | 改动 |
|---|---|
| `use std::time::{...}` | 增加 `Duration` |
| `struct DeviceCredentials` | **新增字段** `issued_at_ms: Option<u64>`，带 `#[serde(default)]` |
| `new_generated()` | 初始化 `issued_at_ms: None` |
| `mark_issued_now()` | **新增**：把 `issued_at_ms` 设为当前时间 |
| `age()` | **新增**：返回 `Option<Duration>`，凭据年龄 |
| `is_usable(ttl_days)` | **新增**：完整性 + TTL 双重判断。`ttl_days == 0` 跳过过期检查；`issued_at_ms == None` 视为过期 |
| `get_asr_token()` | 取到 token 后调 `creds.mark_issued_now()` |

`is_complete()` 保持不变（仍被 `is_usable` 内部使用）。

## src/data/credential.rs（改动较大，建议直接看文件）

| 位置 | 改动 |
|---|---|
| `struct CredentialStore` | `credentials` 从 `Option<..>` 改为 `Mutex<Option<..>>`（refresh 后对后续调用可见）；**新增** `ttl_days: u64` |
| `new()` | 读 `config.asr.credential_ttl_days`；凭据文件读取失败时改为 warn + 忽略，不再 `.ok()` 静默丢弃 |
| `ensure_credentials()` | 改用 `is_usable(ttl_days)`；命中缓存时日志带年龄；未命中时按「不完整 / 已过期 / 无时间戳」分别给出明确日志 |
| `invalidate()` | **新增**：清内存缓存 **并删除 credentials.json**，供会话中途失效时调用 |
| `force_refresh()` | **新增**：丢弃缓存后强制重新注册 |
| `register_and_store()` | **新增**（私有）：注册 + 取 token + 落盘，被 `ensure_credentials` / `force_refresh` 复用。落盘失败只 warn，不中断本次运行 |

## src/asr/client.rs

| 位置 | 改动 |
|---|---|
| `use anyhow::{anyhow, Result}` | 改为 `use anyhow::Result`（`anyhow!` 已全部替换为 `AsrSetupError`） |
| `use std::time::{...}` | 增加 `Duration` |
| `enum SetupFailure` | **新增**：`Timeout` / `StaleCredentials` / `Rejected` |
| `struct AsrSetupError` | **新增**：`{ kind, detail }`，实现 `Display` + `std::error::Error` |
| `AsrSetupError::warrants_credential_refresh()` | **新增**：`Timeout` 或 `StaleCredentials` 时返回 true |
| `is_credential_failure(msg)` | **新增**（`pub`，供 voice_controller 复用）：关键字启发式判定 |
| `struct AsrClient` | **新增字段** `connect_timeout_secs: u64` |
| `new()` | 保持签名不变，默认超时 8 秒 |
| `with_timeout()` | **新增**构造器，`.max(1)` 防止传 0 导致秒失败 |
| `start_realtime()` — 握手 | `connect_async` 包进 `tokio::time::timeout`；超时→`Timeout`，连接错误→`Rejected` |
| `start_realtime()` — StartTask | `read.next()` 加超时；原 `if let Some(Ok(..))` 改为完整 `match`，覆盖非二进制帧 / 传输错误 / 连接关闭 / 超时四种情况 |
| `start_realtime()` — StartSession | 同上 |
| `classify_setup_error()` | **新增**（私有）：按 `is_credential_failure` 决定 `StaleCredentials` 还是 `Rejected` |

音频发送任务与响应接收任务的逻辑未改动。

## src/asr/mod.rs

导出行改为：
```rust
pub use client::{is_credential_failure, AsrClient, AsrSetupError, SetupFailure};
```

## src/audio/capture.rs

| 位置 | 改动 |
|---|---|
| `start()` | **签名不变**，改为 `channel()` + `start_into()` 的包装，保持向后兼容 |
| `channel()` | **新增**（关联函数）：只建 channel 不启动采集，容量 100（与原来一致） |
| `start_into(tokio_tx)` | **新增**：原 `start()` 的主体，改为接收外部 sender，返回 `Result<()>` |

`stop()`、`run_audio_capture()` 及编码逻辑未改动。

## src/data/config.rs

`AsrConfig` 新增两个字段（均带 `#[serde(default = "..."]`，旧配置文件兼容）：

```rust
pub credential_ttl_days: u64,   // default_credential_ttl_days() = 3
pub connect_timeout_secs: u64,  // default_connect_timeout_secs() = 8
```
`impl Default for AsrConfig` 同步更新。

## src/business/voice_controller.rs

| 位置 | 改动 |
|---|---|
| imports | 增加 `tokio::sync::Mutex as AsyncMutex`、`AsrResponse`、`AsrSetupError`、`is_credential_failure`、`CredentialStore` |
| `struct VoiceController` | `asr_client` 改为 `Arc<AsyncMutex<Arc<AsrClient>>>`（refresh 时要换掉）；**新增** `credential_store: Option<Arc<CredentialStore>>`、`connect_timeout_secs: u64`、`credentials_dirty: Arc<AtomicBool>` |
| `new()` | **签名不变**，新字段取默认值（`credential_store: None` → 不启用自动刷新） |
| `with_credential_refresh(store, timeout)` | **新增**：builder 风格，启用自动重注册 |
| `connect_with_retry()` | **新增**（核心）：① 若 `credentials_dirty` 则先重注册；② 首次连接；③ 失败且 `warrants_credential_refresh()` 则 `force_refresh()` + 换 client + 重试一次；④ 返回 `(audio_tx, result_rx)` |
| `start()` | **时序调整**：先 `connect_with_retry()`，成功后才 `audio_capture.start_into(audio_tx)`。两条失败路径都复位 `is_recording` |
| 结果处理任务 `ResponseType::Error` 分支 | **新增**：`is_credential_failure()` 命中时 `store.invalidate().await` + 置 `credentials_dirty`，并提示用户 |

`stop()`、`update_text()`（增量更新算法）未改动。

## src/main.rs

`run_ui_mode()` 与 `run_cli_mode()` 两处同样的三点改动：
1. `CredentialStore::new(&config)?` 包一层 `Arc::new(...)`
2. `AsrClient::new(creds)` → `AsrClient::with_timeout(creds, connect_timeout)`
3. `VoiceController::new(...)` 链上 `.with_credential_refresh(credential_store.clone(), connect_timeout)`

## config.toml / config.toml.example / README.md

`[asr]` 段补两个配置项及中文注释；README 增加「凭据过期与自动重注册」小节，说明三层处理机制。

---

# 第二轮改动（本次会话，无独立 commit / 未生成新 patch）

以上第一轮是 HANDOFF.md 原始交接的内容，`doubao-credential-fix.patch` 只覆盖到这里。
**下面这些改动没有对应的 patch 文件**——这个目录不是 git 仓库，改动只体现在文件本身，
要追溯历史只能靠这份 CHANGES.md。

基线：在第一轮基础上，本机装好 Rust + protoc 工具链后实际编译通过、跑了多轮 `--cli`
和真实设备注册/ASR 连接测试，从「静态审查」推进到「编译 + 实测」。

## .cargo/config.toml（新增文件）

```toml
[target.x86_64-pc-windows-msvc]
rustflags = ["-C", "target-feature=+crt-static"]
```

静态链接 MSVC 运行库，产物不再依赖 `VCRUNTIME140.dll`/`api-ms-win-crt-*.dll`，
`dumpbin /dependents` 验证过只剩 Windows 系统自带 DLL。项目级配置，不影响其他 Rust 项目。

## src/business/hotkey_manager.rs

| 位置 | 改动 |
|---|---|
| `enum HotkeyMode` | **新增** `SingleTap` 变体；派生加了 `Copy` |
| `is_modifier_key()` | **新增**（私有）：判断是否为需要走底层钩子的裸修饰键，覆盖 `ctrl/lctrl/rctrl/shift/lshift/rshift/alt/lalt/ralt` |
| `HotkeyManager::new()` | `mode` 解析加 `"single_tap"` 分支；注册逻辑里 `DoubleTap`/`SingleTap` 合并处理 |
| `HotkeyManager::active_handle()` | **新增**：返回内部 `is_active: Arc<AtomicBool>` 的克隆，供外部（托盘暂停/恢复）在不共享整个 `HotkeyManager`（其含有平台句柄，线程不安全）的前提下控制热键是否生效 |
| `on_trigger()` | `use_keyboard_hook` 判断加入 `SingleTap`；hook 线程新增 `require_double_tap` 参数；`match mode` 分支覆盖 `SingleTap` |
| `run_modifier_double_tap_hook()` → `run_modifier_key_hook()` | **改名+新增参数** `require_double_tap: bool`；`target_vks` 匹配新增 `lctrl/rctrl/lshift/rshift/lalt/ralt`，可以只监听左右某一侧；`HookState` 新增 `require_double_tap` 字段 |
| `keyboard_hook_proc()` | **行为变更（重要）**：命中触发键时，不论按下/抬起，一律 `return LRESULT(1)` **吞掉**该按键，不再调用 `CallNextHookEx`。修复"用 Alt 当热键时，输入框失焦需要重新点击"——根因是 Windows 把单独按 Alt 识别成"激活菜单"手势，之前钩子只旁听不拦截，按键照常传给了前台窗口 |

## src/ui/floating_button.rs

| 位置 | 改动 |
|---|---|
| `FloatingButtonStateSetter::set_visible(bool)` | **新增**：用 `ShowWindow(SW_SHOW/SW_HIDE)` 控制悬浮按钮显隐，供"暂停服务"时隐藏按钮 |

## src/ui/system_tray.rs（改动最大）

| 位置 | 改动 |
|---|---|
| 菜单项 | 全部改成英文；新增 `Service: Pause`/`Service: Resume`（动态切换文案），插在 Start/Stop 和 Settings 之间 |
| `TrayIconEvent` 监听 | **新增**：左键点击托盘图标（`Click { button: Left, button_state: Up }`）触发暂停/恢复，效果与菜单项一致 |
| 暂停/恢复逻辑 | 停止当前录音（如果在录）→ 通过 `hotkey_active`（`HotkeyManager::active_handle()`）禁用/恢复热键 → 悬浮按钮隐藏/显示 → 托盘图标切换灰色/彩色渐变、tooltip 切换文案 |
| 线程模型 | `TrayIcon`/`MenuItem` 底层是 `Rc<RefCell<..>>`，**不是** `Send`。业务逻辑（判断暂停状态、调用 controller）留在原有的事件处理线程；真正调用 `tray_icon.set_icon()`/`service_item.set_text()` 只在创建它们的主线程做，用 `tray_dirty: Arc<AtomicBool>` 标志位通知。主线程额外挂一个不绑窗口的 `SetTimer(None, 0, 150, None)`，保证没有新 Win32 消息时也能定期醒来刷新图标 |
| `load_icon()` | 签名改成 `load_icon(paused: bool)`，`paused=true` 时把紫蓝渐变换成灰色，复用原有麦克风绘制逻辑 |
| Start 菜单项 | 暂停状态下点击会被忽略（避免绕过暂停直接开始录音） |
| 各处 `controller.start()` 调用 | **新增**：调用前立即 `setter.set_state(ButtonState::Processing)`，失败时回退 `Idle`。原来是等 `start()`（含 ASR 握手，实测约 1.5-2.5s）完全返回才有任何视觉反馈，体验上像"卡住了" |

## src/business/text_corrector.rs（新增文件）

用豆包大模型（火山方舟 Chat Completions API，OpenAI 兼容协议）对 ASR **最终确认结果**做纠错：

- `TextCorrector::from_config(&LlmConfig)`：`enabled=false` 或缺 `endpoint_id`/key 时返回 `None`（调用方直接跳过纠错，不是报错）；API key 优先读环境变量 `ARK_API_KEY`，其次读 `config.api_key`
- `TextCorrector::correct(&self, text: &str) -> String`：内部任何失败（超时/网络错误/响应格式不对）都吞掉、记 warn 日志，返回原文——纠错绝不阻塞或吞字
- 请求：`POST {base_url}`，`Authorization: Bearer {key}`，`model = endpoint_id`，system prompt 明确要求"只改同音字/错别字/标点，不改语义，不要解释"
- 已用真实 key/endpoint 实测：故意造的错"石物"→"食物"、"酸略"→"策略"，纠正正确

## src/business/voice_controller.rs

| 位置 | 改动 |
|---|---|
| `VoiceController` | 新增字段 `text_corrector: Option<Arc<TextCorrector>>` |
| `with_text_corrector()` | **新增** builder，`credential_store` 走同样的可选挂载模式 |
| `ResponseType::FinalResult` 分支 | 插入纠错步骤：有 corrector 则 `corrector.correct(&response.text).await` 后再走 `update_text()`；`InterimResult`（实时候选字）不受影响，避免频繁调用+界面闪烁 |
| `start()` | **新增计时**：`start_requested_at = Instant::now()`，在"ASR 连接建立"和"音频采集开始"两处打点 log，用于诊断启动延迟 |

## src/asr/client.rs

`start_realtime()` 新增计时日志：WebSocket 连接、`TaskStarted`、`SessionStarted` 各自相对 `setup_start` 的耗时（ms）。用于定位启动延迟具体花在哪一步。

实测数据（真实网络环境）：WebSocket 握手 ~2000ms（TCP+TLS，占大头）、`TaskStarted` +~220ms、`SessionStarted` +~270ms，总计约 2.5s。

## src/data/config.rs

新增 `LlmConfig` 结构体（`enabled`/`endpoint_id`/`api_key`/`base_url`/`timeout_secs`，均带默认值），`AppConfig` 新增 `llm` 字段。

## src/main.rs

`run_ui_mode()`/`run_cli_mode()` 都加：`TextCorrector::from_config(&config.llm)` 成功则 `.with_text_corrector(Arc::new(..))` 挂到 `VoiceController` builder 链上。

## config.toml / config.toml.example / README.md

- `[hotkey]` 注释更新，说明 `single_tap` 模式和 L/R 修饰键写法
- 新增 `[llm]` 段（`enabled=false` 默认关闭）
- `config.toml`（本机实际使用的那份，跟 `target/release/config.toml` 保持同步）目前是：
  `hotkey.mode="single_tap"` + `double_tap_key="RAlt"`，`llm.enabled=true` +
  真实 `endpoint_id`（key 走环境变量 `ARK_API_KEY`，不在文件里）
- README 新增「大模型纠错」「部署到其他机器」「已知限制」小节，修正技术架构表里
  过时的 "rdev" 描述（实际用的是 `global-hotkey` + 底层键盘钩子）
