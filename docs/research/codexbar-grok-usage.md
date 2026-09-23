# CodexBar / Grok 周限量读取研究

> Historical research, valid as of the date below. For current behavior and upgrade instructions, see the [README](../../README.md).

> 研究日期：2026-08-15（Asia/Shanghai）  
> 复核版本：CodexBar `a0cecb1f1d08dbf26eb11a909dc976f5067030b5`（2026-08-14）；Grok Build `eb267feff13129e568df38fb6fdf0ceb65f735d6`（2026-08-13）。  
> 目标：确认 CodexBar 为什么能显示 SuperGrok 周限量，并判断 Herdr Rust 插件能否在“不读浏览器 Cookie、不开常驻进程”的约束下复用。

## 结论先行

可以实现，而且不需要浏览器 Cookie、不需要 xAI Management API key，也不需要常驻进程。

可复用的路径是 Grok Build 官方 CLI 使用的登录态 billing backend：

```text
GET https://cli-chat-proxy.grok.com/v1/billing?format=credits
Authorization: Bearer <~/.grok/auth.json 中当前登录 token>
X-XAI-Token-Auth: xai-grok-cli
Accept: application/json
```

该请求返回订阅 credits 配置。新 credits 响应包含 `config.creditUsagePercent` 和
`config.currentPeriod.type/start/end`；`type == "USAGE_PERIOD_TYPE_WEEKLY"` 就是周池，
`end` 是下次重置时间。Grok Build 官方源码、官方测试 fixture 和官方 UI 映射都直接验证了
这一字段形状（见[billing extension](https://github.com/xai-org/grok-build/blob/eb267feff13129e568df38fb6fdf0ceb65f735d6/crates/codegen/xai-grok-shell/src/extensions/billing.rs#L30-L109)、[weekly fixture](https://github.com/xai-org/grok-build/blob/eb267feff13129e568df38fb6fdf0ceb65f735d6/crates/codegen/xai-grok-shell/src/extensions/billing.rs#L551-L602)、[weekly label](https://github.com/xai-org/grok-build/blob/eb267feff13129e568df38fb6fdf0ceb65f735d6/crates/codegen/xai-grok-pager/src/views/credit_bar.rs#L38-L47)）。

这里的“不是 API”需要精确定义：它不是 `api.x.ai` 的开发者 API 用量，也不需要 API key；
但技术上仍然是一次 HTTPS billing API 请求。若“不要 API”是指连远程 HTTPS 请求都不允许，
则无法自动得到服务端周限量，只能手工录入或读取网页/浏览器 Cookie。

## xAI 官方事实：周池是什么

xAI 的 Grok FAQ 明确说，付费计划包含一个可跨 Grok 产品共享的 weekly usage pool，池子按
百分比显示并按计划调度重置；Settings → Usage 会展示当前百分比、产品分解和 weekly reset
日期时间（[官方 FAQ：weekly pool](https://docs.x.ai/grok/faq#how-is-my-weekly-usage-measured)、[官方 FAQ：查看 usage](https://docs.x.ai/grok/faq#how-do-i-check-my-usage)）。这正是插件应该显示的“周限量”，而不是 xAI Management API 的团队/API 消费量。

## CodexBar 实际做了什么

### 1. 首选：CLI-proxy billing REST

CodexBar 的 `GrokCreditsProxyFetcher` 明确把默认地址设为
`https://cli-chat-proxy.grok.com/v1/billing?format=credits`，使用本地 `GrokCredentials` 的
Bearer token，并发送 `x-xai-token-auth: xai-grok-cli`；成功后解析 `config.creditUsagePercent`
和 `config.currentPeriod.end`（[CodexBar fetcher](https://github.com/steipete/CodexBar/blob/a0cecb1f1d08dbf26eb11a909dc976f5067030b5/Sources/CodexBarCore/Providers/Grok/GrokCreditsProxyFetcher.swift#L6-L41)、[CodexBar parser](https://github.com/steipete/CodexBar/blob/a0cecb1f1d08dbf26eb11a909dc976f5067030b5/Sources/CodexBarCore/Providers/Grok/GrokCreditsProxyFetcher.swift#L44-L107)）。

CodexBar 的 provider pipeline 在 web fetch 前先尝试这个 CLI-proxy；只有失败才进入旧的
Cookie / bearer / gRPC fallback（[proxy-first pipeline](https://github.com/steipete/CodexBar/blob/a0cecb1f1d08dbf26eb11a909dc976f5067030b5/Sources/CodexBarCore/Providers/Grok/GrokProviderDescriptor.swift#L234-L281)）。因此“CodexBar 能读”最直接的原因不是它破解了网页，而是它复用了 Grok CLI 的登录 token 和 billing backend。

### 2. `~/.grok/auth.json` 是登录态，不是 API key

官方 Grok Build 的 auth 模型把 `key` 定义为登录凭证，另有 `auth_mode`、`user_id`、
`expires_at`、`refresh_token` 等字段（[GrokAuth model](https://github.com/xai-org/grok-build/blob/eb267feff13129e568df38fb6fdf0ceb65f735d6/crates/codegen/xai-grok-shell/src/auth/model.rs#L47-L107)）。默认 Grok CLI 把 proxy base URL 设成 `https://cli-chat-proxy.grok.com/v1`（[official default](https://github.com/xai-org/grok-build/blob/eb267feff13129e568df38fb6fdf0ceb65f735d6/crates/codegen/xai-grok-shell/src/agent/config.rs#L45-L51)、[resolution](https://github.com/xai-org/grok-build/blob/eb267feff13129e568df38fb6fdf0ceb65f735d6/crates/codegen/xai-grok-shell/src/agent/config.rs#L270-L305)）。

官方认证文档说明：登录凭证写入 `~/.grok/auth.json`，默认权限为 Unix `0600`，Grok 会在后台自动刷新 token；没有服务端 expiry 时才使用 30 天 fallback（[官方认证文档](https://github.com/xai-org/grok-build/blob/eb267feff13129e568df38fb6fdf0ceb65f735d6/crates/codegen/xai-grok-pager/docs/user-guide/02-authentication.md#browser-login-default)、[official storage implementation](https://github.com/xai-org/grok-build/blob/eb267feff13129e568df38fb6fdf0ceb65f735d6/crates/codegen/xai-grok-shell/src/auth/storage.rs#L77-L100)）。CodexBar 自己只读缓存 token，不替用户刷新；其文档提示常见 token 大约 7 天过期，刷新由 CLI 负责（[CodexBar auth notes](https://github.com/steipete/CodexBar/blob/a0cecb1f1d08dbf26eb11a909dc976f5067030b5/docs/grok.md#oauth-credentials)）。

### 3. 官方 Grok Build 的 billing extension 证明了周字段

Grok Build 的 `x.ai/billing` extension 在内部调用同一个 CLI proxy：

1. 要求 xAI 登录态；
2. 从 proxy base 拼出 `/billing?format=credits`；
3. 发送 `Authorization: Bearer <auth.key>`、`X-XAI-Token-Auth`、`x-userid`、客户端版本和 client mode；
4. 解析 JSON，并把 credits config 返回给 UI（[官方 handler](https://github.com/xai-org/grok-build/blob/eb267feff13129e568df38fb6fdf0ceb65f735d6/crates/codegen/xai-grok-shell/src/extensions/billing.rs#L200-L288)）。

同一官方源码的测试明确给出新响应形状：

```json
{
  "config": {
    "creditUsagePercent": 42.5,
    "currentPeriod": {
      "type": "USAGE_PERIOD_TYPE_WEEKLY",
      "start": "2026-06-01T00:00:00Z",
      "end": "2026-06-08T00:00:00Z"
    },
    "onDemandCap": {"val": 5000},
    "onDemandUsed": {"val": 300},
    "prepaidBalance": {"val": 1250},
    "isUnifiedBillingUser": true
  }
}
```

测试同时验证了 `creditUsagePercent`、`currentPeriod.type` 和 `currentPeriod.end` 的解析
（[官方 deserialization test](https://github.com/xai-org/grok-build/blob/eb267feff13129e568df38fb6fdf0ceb65f735d6/crates/codegen/xai-grok-shell/src/extensions/billing.rs#L551-L602)）。官方 UI 依据 period type 显示 “Weekly limit” 或 “Monthly limit”，并显示百分比和 reset 时间（[label mapping](https://github.com/xai-org/grok-build/blob/eb267feff13129e568df38fb6fdf0ceb65f735d6/crates/codegen/xai-grok-pager/src/views/credit_bar.rs#L38-L47)、[summary rendering](https://github.com/xai-org/grok-build/blob/eb267feff13129e568df38fb6fdf0ceb65f735d6/crates/codegen/xai-grok-pager/src/views/credit_bar.rs#L98-L116)、[modal rendering](https://github.com/xai-org/grok-build/blob/eb267feff13129e568df38fb6fdf0ceb65f735d6/crates/codegen/xai-grok-pager/src/views/usage_modal.rs#L852-L888)）。

### 4. 为什么还看到 Cookie / gRPC fallback

CodexBar 文档列出旧的 grok.com gRPC-web fallback：POST 空 protobuf 到
`GrokBuildBilling/GetGrokCreditsConfig`，可用浏览器 session cookie；但当前 endpoint 还要求浏览器
持有的 Web Key Exchange (WKE) keypair，单纯 Cookie 可能得到 gRPC status 16 `no-credentials`
（[CodexBar fallback notes](https://github.com/steipete/CodexBar/blob/a0cecb1f1d08dbf26eb11a909dc976f5067030b5/docs/grok.md#L50-L76)）。对应实现的请求头和 protobuf 解析见 [GrokWebBillingFetcher](https://github.com/steipete/CodexBar/blob/a0cecb1f1d08dbf26eb11a909dc976f5067030b5/Sources/CodexBarCore/Providers/Grok/GrokWebBillingFetcher.swift#L80-L207)。

因此我们不应把浏览器 Cookie 方案当作主路径：它会触发 macOS Chromium Keychain 权限，也受 WKE 变化影响。CodexBar 自己也把 CLI-proxy 放在前面，并把 Cookie 仅作为 fallback（[source-order documentation](https://github.com/steipete/CodexBar/blob/a0cecb1f1d08dbf26eb11a909dc976f5067030b5/docs/grok.md#data-sources--fallback-order)）。

## 对 Herdr Rust 插件的可行性判断

### 满足用户约束的方案（推荐）

| 约束 | 方案 | 结论 |
| --- | --- | --- |
| 只要 SuperGrok 周限量 | 读取 `config.creditUsagePercent`，确认 `currentPeriod.type` 为 `USAGE_PERIOD_TYPE_WEEKLY`，读取 `currentPeriod.end` | 可实现 |
| 不用 xAI Management API | 不调用 `api.x.ai`、不读取管理 key；只调用 Grok CLI 的 token-auth billing backend | 可实现 |
| 不读取浏览器 Cookie | 只读 `~/.grok/auth.json` 中当前 `key`，不访问 Chrome/Safari/Keychain | 可实现 |
| 不开常驻进程 | 在 Herdr agent 事件或手动 refresh 时发一次 HTTPS 请求，结果放在插件内存/Herdr 状态；不启动 daemon | 可实现 |
| Rust | 用现有异步 HTTP client（如 `reqwest`/`hyper`，具体依 Herdr plugin SDK）和 `serde` 解析 | 可实现 |

推荐的数据模型：

```text
GrokWeeklyUsage {
  used_percent: f64,       // 0..=100，来自 config.creditUsagePercent
  left_percent: f64,       // 100 - used_percent，仅展示派生值
  resets_at: DateTime,      // currentPeriod.end
  period: Weekly,           // 必须由 currentPeriod.type 确认
  source: "grok-cli-proxy",
  fetched_at: DateTime,
  status: Fresh | Stale | Unavailable,
}
```

实现顺序建议：

1. 解析 `GROK_HOME`（若存在）或 `$HOME/.grok/auth.json`；文件是 scope → credential 的 JSON map。优先 `https://auth.x.ai::...` 的 OIDC entry，跳过缺少 `key` 或已过期 entry；只把 `key` 读入内存，不读取/记录 `refresh_token`。
2. 请求 `https://cli-chat-proxy.grok.com/v1/billing?format=credits`。Header 至少与 CodexBar 相同：Bearer、`xai-grok-cli`、`Accept: application/json`；如能取得 `user_id`，可同时发送官方 CLI 使用的 `x-userid`。不要把 token 放进 URL、日志、错误文本或 Herdr pane。
3. 严格解析 `config.creditUsagePercent`、`config.currentPeriod.type`、`config.currentPeriod.end`。只有 type 明确含 `WEEKLY` 才显示“周限量”；type 缺失时显示 `unknown`，不要根据“距离 reset 7 天”猜测周周期。
4. 用短 TTL/去抖合并高频事件（例如 15–60 秒内只允许一次请求），手动刷新绕过 TTL。缓存最后一次成功快照并显示 `stale`，不要在失败时伪造 0%。
5. 401/403、缺文件、过期 token、JSON schema 变化统一显示 `unavailable`，提示“请运行 `grok login`”；插件不执行登录、不刷新 token、不导入 Cookie。

### 为什么不直接复用 `grok agent stdio`

CodexBar 也实现了 `grok agent stdio` ACP JSON-RPC 的 `initialize` + `x.ai/billing`，但其文档记录 grok 0.1.210 的 agent-stdio surface 返回 `-32601 Method not found`，只能回退到 web/CLI-proxy；此外还遇到 Grok ACP 不解码 `\/` method name 的兼容问题（[CodexBar ACP notes](https://github.com/steipete/CodexBar/blob/a0cecb1f1d08dbf26eb11a909dc976f5067030b5/docs/grok.md#data-sources--fallback-order)）。

ACP 路径不是常驻 daemon——CodexBar 每次 fetch 会 spawn `grok agent stdio`，并在 timeout 时终止子进程（[CodexBar RPC client](https://github.com/steipete/CodexBar/blob/a0cecb1f1d08dbf26eb11a909dc976f5067030b5/Sources/CodexBarCore/Providers/Grok/GrokRPCClient.swift#L3-L7)、[timeouts/teardown](https://github.com/steipete/CodexBar/blob/a0cecb1f1d08dbf26eb11a909dc976f5067030b5/Sources/CodexBarCore/Providers/Grok/GrokRPCClient.swift#L166-L201)）。但对 Herdr 轻量插件来说，直接一条 HTTP 请求更简单、启动更快、失败面更小。

## 限制、稳定性与安全边界

### 稳定性

- `cli-chat-proxy.grok.com/v1/billing?format=credits` 是 Grok Build 官方源码正在使用的 backend，但 xAI 公开文档把它作为 CLI 内部实现而非承诺给第三方的稳定公共 API；字段和鉴权头可能变化。
- 新 credits shape 有 typed `currentPeriod`，旧 shape 可能只有 `monthlyLimit`/`used`/`billingPeriodEnd`。插件必须兼容旧字段，但只把明确的 `USAGE_PERIOD_TYPE_WEEKLY` 当作周限量。
- `productUsage` 目前会按产品拆分（官方 FAQ列出 API、Build、Chat、Imagine、Voice），本需求只显示共享 weekly pool，不应把单一产品比例误当成总周池（[官方 FAQ](https://docs.x.ai/grok/faq#how-do-i-check-my-usage)、[官方 parser test](https://github.com/xai-org/grok-build/blob/eb267feff13129e568df38fb6fdf0ceb65f735d6/crates/codegen/xai-grok-shell/src/extensions/billing.rs#L551-L602)）。
- Team/enterprise principal 可能没有个人周池。CodexBar 明确在 team usage surface 不可用时只保留 identity；插件应显示 `unsupported-team-usage`，不要拿团队 API 用量替代（[CodexBar team handling](https://github.com/steipete/CodexBar/blob/a0cecb1f1d08dbf26eb11a909dc976f5067030b5/docs/grok.md#data-sources--fallback-order)）。

### 安全

- `~/.grok/auth.json` 的 `key` 是可用的账户登录凭证；官方文档警告任何能读该文件的进程都能使用凭证，并要求 owner-only 权限和私有 home（[official credential warning](https://github.com/xai-org/grok-build/blob/eb267feff13129e568df38fb6fdf0ceb65f735d6/crates/codegen/xai-grok-pager/docs/user-guide/02-authentication.md#credential-storage)）。
- 插件只读 `key`，不读取 `refresh_token`，不写回 auth 文件，不打印 token，不把完整响应写入日志；内存中的 token 尽可能使用 zeroize。
- 只允许 HTTPS，并对默认 host 做 allowlist；若未来支持 enterprise proxy，应要求显式配置，不接受响应中的任意 redirect host。
- 不实现 Cookie 导入，不访问 Chromium Keychain，不保存 Cookie。这样正好避开 CodexBar 的 gRPC/WKE fallback 风险。
- 不把“HTTP 200 + 缺少百分比”当作 0%（除非服务端明确给出当前 period 且产品协议确认）；否则应 `Unavailable/Stale`。

### 许可证与复用

CodexBar 源码是 MIT（[CodexBar LICENSE](https://github.com/steipete/CodexBar/blob/a0cecb1f1d08dbf26eb11a909dc976f5067030b5/LICENSE)）；Grok Build 官方源码是 Apache-2.0（[Grok Build LICENSE](https://github.com/xai-org/grok-build/blob/eb267feff13129e568df38fb6fdf0ceb65f735d6/LICENSE)）。Herdr 插件应只复用协议事实、字段定义和架构思路，不复制不必要的实现代码；同时要在 README 中声明这是对 Grok CLI billing backend 的 best-effort 集成，不是 xAI 官方插件。

## 验收建议（给实现任务）

最小可验收集合：

- `weekly` fixture：`creditUsagePercent=42.5`、`currentPeriod.type=USAGE_PERIOD_TYPE_WEEKLY`、正确计算 `left_percent=57.5` 和 reset 时间；
- `monthly` fixture：显示 monthly/unavailable，不误报 weekly；
- 缺 `currentPeriod.type`：不猜周期，保留百分比但标注 unknown；
- 缺 `creditUsagePercent` 的旧 shape：用 `used/monthlyLimit` 兼容，但周期必须来自明确字段或标为 unknown；
- 缺少/过期 auth：不发请求，状态为 unavailable；
- 401/403、网络超时、非 JSON、schema 变化：保留上次快照并标 stale；
- 日志和测试断言中不得出现真实 Bearer token、refresh token、Cookie；
- macOS/Linux 均使用同一 HTTP/JSON 路径，不依赖浏览器或常驻进程。

## 给父任务的直接回答

之前说“没有稳定官方 usage API”不完整：对于 SuperGrok weekly pool，Grok Build 官方 Rust 源码已经公开了 CLI proxy billing 调用，CodexBar 在 2026-08-14 的实现也已经把该路径作为首选。我们可以在 Rust Herdr 插件中直接实现同一条 token-auth 请求；“不能读”的主要原因只会是没有 `grok login` 生成的本地 session token、token 过期、xAI 改变内部 endpoint，或账号是 team principal 没有个人 usage surface。浏览器 Cookie 和常驻进程都不是必要条件。
