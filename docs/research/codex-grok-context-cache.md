# Codex 与 Grok 的 context/cache 展示调研

> Historical research, valid as of the date below. For current behavior and upgrade instructions, see the [README](../../README.md).

研究日期：2026-08-26（Asia/Shanghai）  
范围：本插件当前的 Codex/Grok 采集链路、OpenAI Codex 官方 app-server/TUI、Grok Build 官方 status line，以及成熟的 CodexBar 本地用量实现。  
目标：解释截图中 Codex 只有 quota、没有 `context`/`cache` 的原因，并确定哪些字段可以安全接入 Herdr。

## 结论

截图是当前实现的预期结果，不是 Herdr 丢字段：

- Codex adapter 只请求 `account/rateLimits/read` 和只读的 `thread/list`，后者只用于会话摘要；它没有订阅活动 thread 的 token-usage 事件，也没有扫描 rollout JSONL，因此不会产生 context/cache 快照。[`src/providers/codex.rs`](../../src/providers/codex.rs)
- Grok adapter 只请求 CLI billing endpoint。这个响应提供周额度和 reset 时间，不提供会话 context 或 cache；Grok 的 context/cache 在另一条 status-line 输入链路中。[`src/providers/grok.rs`](../../src/providers/grok.rs)
- 两个 provider 的官方数据源实际上都存在，但来源不同：Codex 要绑定活动 thread，Grok 应消费 status-line stdin。不能把 quota 响应里的字段“推导”成 context/cache，也不应把历史累计当作当前窗口占用。

推荐实现顺序：

1. **Grok 先接 status-line hook。** 官方 payload 已直接给出 model、当前 context 百分比、session 累计 cache read/create；复用 Claude/Agy 的本地 observation/cache 机制，不额外发模型请求。
2. **Codex 再接本地 rollout 增量读取，或等待可绑定的长连接事件。** 若现在就做，按 Herdr 的 `agent_session`（thread id）只定位对应 rollout，取最新 `last_token_usage`；必须用文件 offset/累计高水位去重。不要为了读指标 `thread/resume`，也不要每轮扫描所有 session。
3. 字段缺失就隐藏，绝不显示 `0%`、伪造 `no cached` 或把 quota reset 当 cache expiry。成功快照失败时保留上次值，避免 sidebar repaint/churn。

## 实现状态（2026-08-26）

本轮已按上述边界完成短期方案：刷新时复用一次 `herdr agent list` 的
`agent_session`，Codex 只读取匹配 rollout 的有上限尾部（必要时读取有限的文件头补
`turn_context` 模型），Grok 只读取匹配的 `signals.json` 和 `updates.jsonl` 尾部；直接
命令行刷新没有 pane id 时才回退到有上限的最近 Grok sessions。快照按 session 保存
`model` 与 `ContextUsage`，未知 session 不借用其他 pane 的数据，临时缺失则保留同一
账号的 last-good diagnostics。UI 仍保持 context 倒数第二行、limit 最后一行。

Codex rollout 的 token counters 没有服务端过期时间。若最新一次
`last_token_usage` 带有 cache read/write 且事件有时间戳，adapter 会按 OpenAI
[Prompt Caching](https://openai.com/index/api-prompt-caching/) 文档中的“最长一小时”
保留上限计算 `ttl≈...`；这只是本地上限估计，不是精确的 eviction deadline。Grok 的
xAI 文档说明 cache entry 可因负载或重启随时驱逐，因此仍隐藏 TTL，不把 cache hit
时间戳或 billing reset 当成过期时间。

## Codex：官方可用字段与当前缺口

### 官方 app-server token-usage contract

OpenAI 的 `thread/tokenUsage/updated` schema 明确包含：

- `last`：最近一次/当前活动 context 的 token breakdown；
- `total`：线程累计 token breakdown；
- `modelContextWindow`：模型 context window；
- breakdown 中的 `inputTokens`、`cachedInputTokens`、`cacheWriteInputTokens`、`outputTokens`、`totalTokens`。

来源：[官方 `ThreadTokenUsageUpdatedNotification` schema](https://github.com/openai/codex/blob/main/codex-rs/app-server-protocol/schema/json/v2/ThreadTokenUsageUpdatedNotification.json)。

官方 app-server README 说明 token usage 是独立的实时通知流：客户端在 turn 期间消费 `thread/tokenUsage/updated`；`thread/resume` 才会把已持久化的 usage replay 给新的连接。[官方 app-server README：turn events](https://github.com/openai/codex/blob/main/codex-rs/app-server/README.md#turn-events)、[官方 app-server README：resume/replay](https://github.com/openai/codex/blob/main/codex-rs/app-server/README.md#resuming-threads)

因此，单独启动一个一次性 `codex app-server --stdio`、读取 `rateLimits` 后立即退出，无法知道另一个已在运行的 pane 的活动 thread usage。调用 `thread/resume` 会加载/重新加入 thread，属于有状态操作，不符合本插件 quota-only watcher 的边界。

### 官方 TUI 的 context 口径

官方 TUI 源码把 `last_token_usage.total_tokens` 解释为“当前 active context”，把 `total_token_usage.total_tokens` 解释为“累计 session total”；context 剩余百分比使用 `model_context_window`，并预留 12,000-token baseline。它还把 `cached_input_tokens` 作为 input 的子集显示为 `(+ N cached)`。[官方 TUI `token_usage.rs`](https://github.com/openai/codex/blob/main/codex-rs/tui/src/token_usage.rs)

对 Herdr 的含义：

```text
current_context_used = last.total_tokens
context_percent = (current_context_used - 12_000)
                  / (model_context_window - 12_000) * 100
cache_hit_percent = last.cached_input_tokens / last.input_tokens * 100
```

上式只有在 `modelContextWindow > 12_000` 且 `inputTokens > 0` 时才成立；百分比必须 clamp 到 0–100，并标记为 provider 原生口径。`cacheWriteInputTokens` 虽然在 app-server schema 中存在，但 Codex 当前 Responses 解析链路可能丢弃它，不能把 0 解释为“没有 cache write”。[Codex 官方 issue：`cache_write_tokens` telemetry gap](https://github.com/openai/codex/issues/32479)

### 当前 `thread/list` 不能给 model/context

当前 `Thread` schema 的线程元数据有 `modelProvider`，没有活动 model、token usage 或 context usage；`thread/list` 适合历史列表，不是实时 context API。[官方 `ThreadListResponse` schema 的 `Thread` 定义](https://github.com/openai/codex/blob/main/codex-rs/app-server-protocol/schema/json/v2/ThreadListResponse.json#L888-L1080)

`thread/start`/`thread/resume` 的响应会有 model，但这不能解决现有 pane 的活动 thread 关联问题；活动 turn 的真实模型还可能通过 rollout 的 `turn_context` 发生切换。若以后读取 rollout，应以当前 turn 的 `turn_context.payload.model` 为准，而不是把全局默认 model 当成当前模型。

### 高质量实现：CodexBar 的分层做法

[CodexBar 的 `CodexStatusSnapshot`](https://github.com/steipete/CodexBar/blob/main/Sources/CodexBarCore/Providers/Codex/CodexStatusProbe.swift) 的 live snapshot 只保存 credits、5h/weekly 百分比和 reset 时间；它没有把 `/status` 结果伪装成 context/cache。

CodexBar 的另一条 **可选、本地、历史用量** 路径才读取：

- `~/.codex/sessions/YYYY/MM/DD/*.jsonl`；
- `~/.codex/archived_sessions/*.jsonl`；
- `event_msg` 的 `token_count` 和 `turn_context`；
- 解析结果写入本地 cache，供 cost/history 使用。

具体说明见 [CodexBar Codex usage 文档](https://github.com/shaneholloman/codexbar/blob/main/docs/codex.md#local-codex-cost-estimates)。这是一种值得借鉴的分层：live quota 与本地历史 cost 分开；它不是“每个 pane 的实时 context HUD”。

如果 Herdr 要在当前版本显示 Codex context/cache，最小安全实现是只读与 pane `agent_session` 对应的 rollout 文件：

1. 先按 thread id 找文件，不能把所有 rollout 聚合后显示到每个 pane；
2. 从末尾反向读有限窗口，找到最新有效 `token_count`；
3. 使用 `last_token_usage`，不是 `total_token_usage`；
4. 以文件 offset 或累计 `total_token_usage` 高水位去重，因为 Codex 可能重复广播未变化的 token_count；
5. 发现 fork/subagent 继承前缀时，不把父线程前缀重复计入子线程；
6. 找不到活动文件、window 缺失、计数无效时隐藏字段并保留上次成功快照。

这条路径读的是本地会话内容元数据，仍有隐私和 I/O 成本；它应是显式的 bounded local read，不应放进每个 pane 的高频全量 watcher。长期更优方案是接入官方 app-server 的活动 thread 长连接，让 `thread/tokenUsage/updated` 按 thread id 写入本地 observation。

## Grok：官方 status-line 已经提供完整字段

### 官方 status-line payload

Grok Build 官方 status-line 文档给出的内置默认项就是 `cwd`、`model`、`context`，示例为 `Grok 4.5 │ 12% ctx`。命令型 status line 从 stdin 接收 JSON；它是事件驱动的，只有显式设置 `refresh_interval` 才增加定时运行。[Grok Build 官方 status-line 文档](https://github.com/xai-org/grok-build/blob/main/crates/codegen/xai-grok-pager/docs/user-guide/25-status-line.md#status-line)

可直接消费的字段包括：

| 字段 | 语义 |
| --- | --- |
| `session_id` | 稳定的会话 id，用于本地缓存隔离 |
| `model.id` / `model.display_name` | 当前模型标识和可读名称 |
| `context_window.context_window_size` | 当前模型 context 上限 |
| `context_window.context_tokens` | 当前窗口实际占用的 input tokens；compact 后会下降 |
| `context_window.used_percentage` / `remaining_percentage` | 当前窗口已用/剩余百分比 |
| `context_window.session_usage.input_tokens` | 整个 session 的累计 uncached input |
| `context_window.session_usage.cache_creation_input_tokens` | session 累计 cache creation |
| `context_window.session_usage.cache_read_input_tokens` | session 累计 cache read |
| `transcript_path` | Grok 自己的 `updates.jsonl`，不是 Claude transcript |

字段语义和“缺失就不发送”的规则见 [官方 status-line available data](https://github.com/xai-org/grok-build/blob/main/crates/codegen/xai-grok-pager/docs/user-guide/25-status-line.md#available-data)。Grok 官方还说明 `session_usage` 是累计 session 口径，不是单次请求；`context_tokens` 才是当前窗口口径。

建议的 Grok 展示：

```text
context = used_percentage
cache_hit = cache_read_input_tokens
             / (input_tokens + cache_creation_input_tokens + cache_read_input_tokens)
```

当分母为 0 或字段缺失时隐藏 cache。不要把 `session_input_tokens / context_window_size` 当 context：session 累计 token 会超过 100%，官方明确把它与当前 `context_tokens` 区分开。

### 官方 status-line 的刷新/失败行为可直接借鉴

官方文档规定：

- command status line 的 `refresh_interval` 会把“上次 payload”再次传给脚本；定时触发时 payload 中的 session 数字不是新的，只有脚本主动取到的外部数据才新鲜；
- 网络脚本应使用较长间隔并读本地 cache，避免忙碌 turn 形成请求风暴；
- 定时运行失败会保留上次输出；
- 字段不可用时省略，而不是发送 `null`/占位符。

这与本插件的安全目标一致：让 Grok status-line hook 负责采集一次 payload，插件只保存 observation；watcher 只刷新现有快照，不再单独扫描 pane 或发额外模型请求。

### Grok 本地 session 文件是可行但次优的 fallback

Grok 官方 shell 文档公开了本地布局：`~/.grok/sessions/<encoded-cwd>/<session-id>/summary.json`、`updates.jsonl`、`signals.json` 等；`signals.json` 保存 token usage 等 session signals。[官方 Grok shell storage layout](https://github.com/xai-org/grok-build/blob/main/crates/codegen/xai-grok-shell/README.md#storage-layout)、[官方 sessions 文档](https://github.com/xai-org/grok-build/blob/main/crates/codegen/xai-grok-pager/docs/user-guide/17-sessions.md#storage-layout)

本地 `signals.json` 确实可以提供 context/model 相关的 session signals，但路径按 cwd 编码、session 目录会 fork/subagent，且这是 CLI 内部持久化格式。优先级应低于 status-line stdin：只有 status-line 没安装或历史恢复场景，才考虑按明确 session id 读取一个 signals 文件；不能扫描所有 Grok session 后把最新值广播给所有 pane。

## 参照方案与本插件的落地边界

| Provider | 最可靠的实时来源 | 可显示的 context | 可显示的 cache | 目前为何看不到 |
| --- | --- | --- | --- | --- |
| Codex | 活动 thread 的 `thread/tokenUsage/updated`；短期可 bounded-read 对应 rollout | `last.total_tokens / modelContextWindow`，按官方 12k baseline 计算 | `cachedInputTokens / inputTokens`；cache write 仅在明确非零时展示 | 当前只读 rate limits + thread list，不绑定活动 thread |
| Grok | 官方 status-line stdin | `context_window.used_percentage` | session_usage 的 read/create/fresh 累计比率 | 当前只读 billing endpoint，没有安装 Grok status-line observation |

实现时应保持以下边界：

- 不 resume Codex thread、不启动 warm-up turn、不为了 context/cache 请求模型；
- 不把 Codex `total_token_usage` 或 Grok `session_input_tokens` 当当前 context；
- 不把 billing quota 的 reset 时间当 cache TTL；两者是不同资源；
- context 的显示口径写清楚“used”，cache 写 `cache N.N%`，避免把剩余/已用混淆；
- provider 快照按 session/thread 隔离，同一 provider 的多个 pane 不得互相覆盖；
- `visible` Herdr pane 读取规则不变：指标采集走本地 statusline/文件，不为此读取 pane；
- 任何缺失或不可靠字段都隐藏，保留 last-good snapshot，避免 metadata repaint。

## 参考来源

- [OpenAI Codex app-server README](https://github.com/openai/codex/blob/main/codex-rs/app-server/README.md)
- [OpenAI `ThreadTokenUsageUpdatedNotification` schema](https://github.com/openai/codex/blob/main/codex-rs/app-server-protocol/schema/json/v2/ThreadTokenUsageUpdatedNotification.json)
- [OpenAI Codex TUI token usage implementation](https://github.com/openai/codex/blob/main/codex-rs/tui/src/token_usage.rs)
- [OpenAI `ThreadListResponse` schema](https://github.com/openai/codex/blob/main/codex-rs/app-server-protocol/schema/json/v2/ThreadListResponse.json)
- [OpenAI cache-write telemetry issue](https://github.com/openai/codex/issues/32479)
- [CodexBar live Codex status probe](https://github.com/steipete/CodexBar/blob/main/Sources/CodexBarCore/Providers/Codex/CodexStatusProbe.swift)
- [CodexBar local Codex usage/cost scanner](https://github.com/shaneholloman/codexbar/blob/main/docs/codex.md#local-codex-cost-estimates)
- [Grok Build status-line guide](https://github.com/xai-org/grok-build/blob/main/crates/codegen/xai-grok-pager/docs/user-guide/25-status-line.md)
- [Grok Build session storage layout](https://github.com/xai-org/grok-build/blob/main/crates/codegen/xai-grok-shell/README.md#storage-layout)
- [Grok Build sessions guide](https://github.com/xai-org/grok-build/blob/main/crates/codegen/xai-grok-pager/docs/user-guide/17-sessions.md#storage-layout)
