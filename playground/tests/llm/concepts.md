# 后端调用大模型：概念和分层

后端怎么调大模型，tool、MCP、skill、harness 这些词分别指什么、在哪一层，rig 覆盖了哪几层。
rig 相关的内容按 0.46.0 核对（2026-11）。

## 本质：调一个 HTTP 接口

模型跑在厂商（OpenAI、DeepSeek……）的服务器上，后端发 HTTPS POST，请求和响应都是 JSON：

```
POST https://api.openai.com/v1/chat/completions
Authorization: Bearer <API key>

{
  "model": "gpt-4o-mini",
  "messages": [
    {"role": "system", "content": "你是客服，回答要简短"},
    {"role": "user",   "content": "退货要几天？"}
  ]
}
```

```json
{
  "choices": [{"message": {"role": "assistant", "content": "一般 3～5 个工作日。"}}],
  "usage": {"prompt_tokens": 25, "completion_tokens": 12}
}
```

- `messages` 是对话记录，每条有一个角色：`system` 定规则、人设，`user` 是用户说的话，`assistant` 是模型之前的回答，`tool` 是工具执行的结果。
- `usage` 是这次用了多少 token，费用按 token 算，输入和输出分开计价。
- OpenAI 的协议基本是通用标准：DeepSeek、通义千问、Kimi、智谱、OpenRouter，以及本地的 Ollama、vLLM 大多提供兼容接口，换个 base URL 和 key 就能调。

### 服务端不记对话

接口无状态，多轮对话要后端自己存历史（数据库、Redis），每次请求带上之前的 `user` / `assistant` 消息再加上新问题。
历史越长，每次花的 token 越多，长对话要截断或压缩。

### 流式输出

生成一段回答要几秒到几十秒。请求里加 `"stream": true`，服务端用 SSE 一段一段地推，后端接住再用 SSE 或 WebSocket 转给前端。

### 工具调用（tool / function calling）

让模型能用后端的功能（查订单、下单）：

1. 请求时带上后端提供的函数：名字、用途、参数格式（JSON Schema）。
2. 模型需要时不直接回答，而是返回「调用 `query_order`，参数 `{"order_id": "123"}`」。
3. **后端执行这个函数**，把结果作为一条 `tool` 消息追加进对话，再请求一次。
4. 模型看到结果生成最终回答；中间可能连续要求调好几个工具。

```json
{
  "name": "query_order",
  "description": "按订单号查询订单状态",
  "parameters": {
    "type": "object",
    "properties": {"order_id": {"type": "string"}},
    "required": ["order_id"]
  }
}
```

模型只看得到这段描述，选不选这个工具、填什么参数全靠 `description` 写得清不清楚。模型从不执行代码，执行的永远是调用方的程序。

### 结构化输出

请求里给一个 JSON Schema，要求模型按格式返回，后端直接反序列化成结构体。用于从文本里提取字段、做分类这类要接着跑程序逻辑的场景。

### embedding 和 RAG

- embedding 接口把一段文字转成一串数字（向量），意思相近的文字向量也相近。
- RAG：文档切成小段、转成向量存进向量库；用户提问时把问题也转成向量，找出最相关的几段塞进 prompt，模型就能回答它训练时没见过的资料。

### 后端要注意的

- key 只放后端，从环境变量或配置读，不下发到前端。
- 超时设长：一次调用几秒到几十秒是正常的。
- 限流：超了返回 429，退避重试。
- 成本：按 token 计费，记下每次的 `usage`。
- 输出不可信：可能胡编、不按格式，结果要校验；工具调用的参数当成用户输入一样检查。

## tool、MCP、skill

三者是一层层叠上去的：tool 是底层机制，MCP 是工具怎么标准化地接进来，skill 是说明和资料怎么按需给模型。

### MCP（Model Context Protocol）

工具写在自己的程序里时，想让 AI 查 GitHub、查数据库、操作 IDE，每个 AI 应用都要自己对接一遍。MCP 把「提供工具的一方」拆成单独的程序：

- **MCP server** 声明自己提供哪些工具（还可以提供资源、提示词模板）。
- **MCP client**（AI 应用）连上它（本地 stdio，远程 HTTP），先问有哪些工具，把描述转给模型；模型要调用时 client 转发给 server 执行，结果再给回模型。
- 对模型来说和普通工具没区别，MCP 只解决工具从哪来、怎么接。一个 server 写一次，Claude Code、Cursor、自己的后端都能接。

### skill

说明（系统提示词、工具描述）每次请求都要带上，占 token，也分散模型注意力；把各种规范、流程全塞进系统提示词，大部分时候用不上。

一个 skill 是一个文件夹：`SKILL.md`（开头是名字和一句话描述，后面是详细步骤），还可以带脚本、模板、参考资料。

- 平时只把每个 skill 的名字和描述放进上下文。
- 模型判断用得上时才读完整的 `SKILL.md`，再按需读别的文件、跑脚本。

skill 不是接口协议，是按需加载说明和资料的组织方式。它要求 agent 能读文件（带脚本的还要能执行），所以主要在 Claude Code 这类 agent 环境里用。
在自己的后端里要类似效果，一般按场景挑出相关的提示词和资料拼进请求，或者用 RAG 检索。

## 分层：从模型到产品

自下往上，每一层套在下一层外面：

```
⑤ 产品          Claude Code、ChatGPT、Cursor        给人用的界面、账号、计费
④ agent harness  Claude Code 的内核、rig-harness     让模型长时间自主干活的运行环境
③ 应用框架       rig、LangChain                     agent 循环、工具、RAG 的积木
② SDK / 客户端   async-openai、genai                把 HTTP 接口包成函数和类型
① 模型 API       OpenAI / Anthropic 的 HTTP 接口    发 messages，收回答
```

- **① 模型 API**：看到一串消息，生成下一条回复（文字，或者「调用某个工具」）。不执行任何东西，也不记任何东西。
- **② SDK / 客户端**：把 JSON 包成类型，处理鉴权、流式、报错，不加额外逻辑。
- **③ 应用框架**：agent（自动跑工具调用循环）、工具定义、结构化输出、embedding、向量库、RAG、统一多家厂商的接口。用不用、怎么组合由调用方的代码决定。
- **④ agent harness**：见下一节。
- **⑤ 产品**：harness 加上界面、账号、计费。

### harness

harness 原意是马具：模型是马，harness 是让它能干活又不乱跑的那套装置，即把模型包起来、让它能长时间自主完成任务的程序。以 Claude Code 为例，模型以外的部分都是 harness：

| harness 负责的 | Claude Code 里的样子 |
|---|---|
| agent 循环：调模型 → 执行工具 → 给回结果 → 再调，直到完成 | 一次回复里连着跑好几条命令 |
| 工具的具体实现 | `Read`、`Edit`、`Bash` 真正读文件、跑命令 |
| 接 MCP server、按需加载 skill | 连 IDE 的 MCP server；skill 列表 |
| 上下文：拼系统提示词、读规则文件，太长时压缩 | 自动读 `CLAUDE.md` / `AGENTS.md`；长会话自动总结 |
| 权限和沙箱 | 跑某些命令前弹窗确认 |
| 会话和记忆 | 保存、恢复会话；跨会话的 memory 目录 |
| 子 agent | 把子任务派给另一个模型实例 |

同一个模型换个 harness，表现可能差很多。常说的 context engineering（上下文工程）主要是 harness 的工作：每次请求里放什么、不放什么、放在哪。

## 常见的词

| 词 | 意思 | 层 |
|---|---|---|
| prompt / system prompt | 发给模型的输入；system prompt 是定规则、人设的那条 | ① |
| token | 计费和长度的单位，中文大约一两个字一个 | ① |
| context window | 一次请求最多能放多少 token，历史、资料、工具描述都算 | ① |
| tool / function calling | 模型返回调用请求，程序执行 | ① 定格式，③④ 执行 |
| structured output | 让模型按 JSON Schema 返回 | ① |
| embedding / 向量库 / RAG | 文字转向量，检索相关资料塞进 prompt | ① 提供 embedding，③ 组装 |
| agent | 模型 + 工具 + 自动循环 | ③ 积木，④ 完整 |
| workflow | 步骤由代码写死、模型只做其中几步；agent 是模型自己决定下一步 | ③ |
| subagent / multi-agent | 一个 agent 把子任务派给另一个 | ③④ |
| memory / compaction | 跨轮次、跨会话记东西；对话太长时压缩历史 | ③④ |
| MCP | 工具从外部程序接进来的标准协议 | ③④ 作为 client |
| skill | 按需加载的说明和资料 | ④ |
| guardrails | 检查输入输出，拦住不该说、不该做的 | ③④ 或自己的代码 |
| evals | 用一批题目测模型或 prompt 的效果 | 开发流程 |
| tracing / observability | 记录每次调用的 prompt、输出、token、耗时 | 各层 |

## rig 覆盖的层

rig 横跨 ②～④，既是库，也是一个终端 coding agent（类似 Claude Code）：

| crate | 做什么 | 层 |
|---|---|---|
| `rig-core` | 各厂商的协议、消息、工具、向量库这些底层定义 | ② |
| `rig-agent` | 经典 agent 运行时：builder、工具调用循环、流式、结构化输出 | ③ |
| `rig-memory` | 对话历史、压缩、保存和恢复会话 | ③④ |
| `rig-rmcp` | 作为 MCP client，把 MCP 工具接成 rig 的工具 | ③ |
| `rig-tools` | coding agent 用的工具：读写文件、搜索、跑 shell | ④ |
| `rig-harness` + `rig-cli` | 它自己的终端 coding agent（`cargo install rig`） | ④⑤ |
| `rig`（facade） | 应用直接依赖它，默认带 `rig-agent` 和 reqwest | 入口 |

- 依赖写 `rig`（`cargo add rig`），不写 `rig-core`：`rig-core` 只剩底层定义，默认 feature 里也没有 HTTP 客户端。
- 写法：`OpenAI::from_env()?.completion(模型)` 拿到 model，`AgentBuilder::new(model)` 建 agent，`.prompt(..)` 提问。
- 最低 Rust 版本 1.97.1；还在 0.x，几乎每个版本都有破坏性改动，仓库的 `MIGRATING.md` 每次发版写迁移说明。
- 内置约 40 家厂商，没有按 feature 拆开，只用一家也会全部编译。

后端调 AI 一般用到 ③：服务决定什么时候调模型、给哪些工具、怎么处理结果。harness 适合「给一个开放目标、让 AI 自己用一堆工具做完」的场景（写代码、做调研），
这类可以直接用把 harness 做成库的 SDK，比如 Anthropic 的 Claude Agent SDK、OpenAI 的 Agents SDK。

## Rust 里的选择

| crate | 层 | 适合 |
|---|---|---|
| async-openai | ② | 只调 OpenAI 或兼容 OpenAI 协议的服务，类型完整 |
| genai | ② | 一套接口接多家厂商 |
| rig | ②～④ | 要 agent、工具、RAG、记忆这些现成积木 |
| reqwest 直接调 | — | 只用一两个接口 |
