# AI 审查残缺 JSON 可观测性与恢复设计

## 背景

AI 审查供应商可能返回 HTTP 200，但最终 `submit_review_findings` 参数或
assistant content 是被截断的 JSON。当前实现只重试传输、超时和部分 HTTP
错误；最终 JSON 解析失败会直接使该 AI 审查任务失败。同时，响应模型没有保留
`finish_reason` 和 token usage，日志无法区分输出长度限制与供应商生成异常。

## 目标

- 记录首个 choice 的 `finish_reason` 以及响应 token usage。
- 保持对不返回这些可选字段的 OpenAI 兼容供应商的兼容性。
- 仅在最终 findings 发生 `AiResponseParseFailed` 时执行一次恢复请求。
- 恢复请求不得重放残缺 tool call，也不得形成无限重试。
- 第二次仍失败时保留现有结构化错误行为。
- 不把缺失、冲突或语义无效的 findings 误判为 clean review。
- 对响应体和最终候选数量设置统一上限。

## 非目标

- 不新增或调整 `max_tokens`、`max_completion_tokens` 配置。
- 不尝试补齐、修剪或猜测残缺 JSON。
- 不改变 HTTP、超时、工具预算以及 diff-only fallback 的既有重试语义。
- 不重跑已经完成的上下文工具调用。
- 不合并相互冲突的多个最终 findings payload。

## 设计

### 响应元数据

`OpenAiChoice` 新增可选 `finish_reason`。`OpenAiChatResponse` 新增可选 usage，
usage 内的 prompt、completion 和 total token 均为可选值。缺失或供应商返回
部分 usage 时仍可成功反序列化。

每次准备处理响应 choice 时输出结构化日志，至少包含：

- AI review、model、attempt 和 batch 标识；
- `finish_reason`；
- prompt、completion、total token；
- assistant content 或 tool arguments 的字节数。

未知或缺失字段记录为空，不作为错误。

### 有界响应读取

AI HTTP 响应体最多读取 4 MiB。超过上限时返回结构化
`AiResponseParseFailed`，不继续反序列化，也不进入 malformed-finalization
retry。该上限同时约束初始请求和 context follow-up，避免单个超大 payload 在
HTTP body、OpenAI DTO 和规范化 findings 之间形成内存放大。

### 统一最终候选管线

最终消息只通过一个有界管线处理：

1. 按消息顺序发现所有 `submit_review_findings` arguments；
2. 对非空 assistant content，从每个 `{` 独立尝试启动 JSON object 解析；
3. tool 与 content 共用最多 8 个最终候选，超过上限直接返回协议错误；
4. 每个候选独立完成 JSON 结构解析和 finding 语义验证；
5. 收集全部合法候选并进行规范化一致性比较；
6. 没有合法候选时返回最后一个解析错误；多个合法候选不一致时返回
   `AiResponseParseFailed`；全部一致时返回首个候选的原始顺序。

content 扫描不在 JSON object 外维护引号或大括号状态。每个 `{` 都是独立且有界
的解析起点，因此前置残缺引号或未闭合 object 不会吞掉后续合法候选。

finding 必须满足：

- `path`、`title`、`message` 去除空白后非空；
- `line >= 1`；
- `severity` 字段必须存在，且忽略大小写后严格等于 `error`。

工具 Schema 同步声明字符串 `minLength: 1`、行号 `minimum: 1` 和唯一支持的
severity。规范化比较对 findings 的副本按 path、line、title、message 和 severity
排序并去重，避免仅因顺序不同产生冲突；最终输出保留首个候选的原始顺序。

### 定向 finalization 重试

最终消息进入 `parse_openai_message` 后，如果错误码是
`AiResponseParseFailed`，且本批尚未执行过 malformed-finalization retry：

1. 丢弃该残缺 assistant 消息，不把它加入对话历史；
2. 保留原始 system/user 消息和已成功完成的工具上下文；
3. 追加一条 `role=system` 的可信指令，说明上次最终参数不是完整 JSON，要求立即重新调用
   `submit_review_findings`，压缩描述并放弃证据不足的问题；
4. 关闭 `read_file`、`search_code` 和 `list_files`，只提供
   `submit_review_findings`；
5. 发起一次新的 finalization HTTP 请求。

重试响应正常时返回 findings。重试响应仍无法解析时直接返回第二次的
`AiResponseParseFailed`，不再重试。

若当前是无 tool-call 的 JSON content fallback，则同样只进行一次定向请求，
继续使用 `response_format=json_object`，并要求重新输出完整、精简的 JSON。

### 错误边界

只对“至少存在一个最终候选，但所有候选均结构或语义无效”的情况启用该恢复路径。
外层 OpenAI 响应无法解析、无 choices、无最终候选、候选超限、响应体超限、非
2xx、超时和其他错误继续沿用现有处理逻辑，避免扩大重试范围或掩盖协议错误。
多个合法但不一致的候选属于明确冲突，直接返回 `AiResponseParseFailed`，不消耗
malformed-finalization retry。

## 测试

遵循 TDD 增加以下覆盖：

1. OpenAI 响应可以解析完整和缺失的 finish reason/usage。
2. 首次最终 findings 残缺、第二次完整时，仅发出一次恢复请求并返回 findings。
3. 两次最终 findings 均残缺时返回解析错误，总请求次数有明确上限。
4. 恢复请求不包含残缺 assistant tool call，且不再暴露上下文工具。
5. 正常完整响应不产生额外请求。
6. content 内空与非空合法 JSON 冲突时不返回 clean；语义无效候选后仍可使用合法候选；
   两个相同候选成功。
7. severity 缺失、空、warning 或未知值均失败；不同 findings 顺序仍视为一致。
8. tool 与 content 候选共享 8 个上限；前置残缺引号或 object 不阻断后续合法 JSON。
9. 超过 4 MiB 的初始或 follow-up 响应体被拒绝。
10. malformed recovery 指令以 system role 发送。

完成后运行格式检查、相关单元测试、完整 `cargo test` 和 Clippy。
