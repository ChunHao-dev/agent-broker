# RFC: openAB Cron Job 排程功能

## 摘要

讓 Discord 使用者透過自然語言建立、管理和執行排程任務。AI agent 辨識排程意圖後，透過工具管理 job，openAB 負責實際的排程與執行。

## 動機

目前 openAB 是純被動的 — 只有使用者發訊息時才會回應。沒有辦法設定「每小時報天氣」或「30 分鐘後提醒我」這類排程任務。加入 cron job 支援能讓 openAB 成為真正的持續性助手。

---

## OpenClaw 的做法

OpenClaw 已有成熟的 cron job 系統。以下是它的完整流程：

### 架構

```
使用者：「每天早上 9 點報天氣」
    │
    ▼
┌─────────────────────────────────────────────────────────────────┐
│  AI Agent（主對話 session）                                      │
│                                                                 │
│  1. 辨識排程意圖                                                 │
│  2. 呼叫內建 cron tool（tool calling，JSON 參數）                 │
│     {                                                           │
│       action: "add",                                            │
│       job: {                                                    │
│         schedule: { kind: "cron", expr: "0 9 * * *",            │
│                     tz: "Asia/Taipei" },                        │
│         payload:  { kind: "agentTurn",                          │
│                     message: "check weather" },                 │
│         delivery: { mode: "announce" }                          │
│       }                                                         │
│     }                                                           │
└──────────────────────┬──────────────────────────────────────────┘
                       │ tool call → runtime 攔截
                       ▼
┌─────────────────────────────────────────────────────────────────┐
│  cron-tool.ts（in-process）                                      │
│                                                                 │
│  3. normalize 層修復 AI 輸入（欄位搬移、大小寫、型別轉換）          │
│  4. 補上 agentId、sessionKey                                     │
│  5. 透過 WebSocket JSON-RPC 打到本機 Gateway                     │
└──────────────────────┬──────────────────────────────────────────┘
                       │ ws://127.0.0.1:18789
                       ▼
┌─────────────────────────────────────────────────────────────────┐
│  Gateway（WebSocket server，獨立 process）                        │
│                                                                 │
│  6. 從 session context 自動推斷 delivery 目標                     │
│     （使用者在 Telegram → 結果送回 Telegram）                      │
│  7. 寫入 JSON 持久化                                             │
└──────────────────────┬──────────────────────────────────────────┘
                       │
                       ▼
┌─────────────────────────────────────────────────────────────────┐
│  排程引擎（setTimeout 迴圈）                                      │
│                                                                 │
│  8. 每次 tick 找到期的 job                                        │
│  9. 根據 payload.kind 決定執行模式：                               │
│     ├─ systemEvent → 注入主對話 session（有上下文）                 │
│     ├─ agentTurn  → 開獨立 session（無上下文，可設 model/timeout）  │
│     └─ script     → 直接跑 shell command                         │
│  10. 執行完成後：                                                 │
│      ├─ 更新狀態（nextRunAtMs, lastRunStatus）                    │
│      ├─ 寫 run log（status, duration, token usage）              │
│      ├─ 失敗 → 指數退避重試                                      │
│      └─ 連續失敗 N 次 → 發送 failureAlert                        │
└──────────────────────┬──────────────────────────────────────────┘
                       │
                       ▼
┌─────────────────────────────────────────────────────────────────┐
│  投遞層（與執行層解耦）                                           │
│                                                                 │
│  11. 根據 delivery.mode 投遞結果：                                │
│      ├─ announce → 送到聊天視窗（Telegram/WhatsApp/Slack/...）    │
│      ├─ webhook  → HTTP POST 到指定 URL                          │
│      └─ none     → 只記錄，不送                                   │
└─────────────────────────────────────────────────────────────────┘
```

### 關鍵設計

- **工具暴露方式**：內建 tool（`cron`），不是 MCP，是 OpenClaw 自己的 tool 系統（跟 `exec`、`message` 同層級）。AI 透過 tool calling 呼叫，runtime 攔截後經 normalize 修復輸入，再透過 WebSocket JSON-RPC 打到本機 Gateway process
- **投遞目標**：從 session context 自動推斷（session store 記錄 lastChannel、lastTo、lastThreadId），AI 不需要帶任何 ID
- **排程類型**：`at`（一次性絕對時間）、`every`（固定間隔）、`cron`（cron 表達式 + 時區）
- **執行模式**：三種並存 — 主對話注入（`systemEvent`）、獨立 session（`agentTurn`）、直接 script
- **可靠性**：指數退避、超時保護、失敗通知、重啟補跑（節流式）、並行控制

---

## openAB 的提案做法

### 架構

```
使用者：「每小時報天氣」
    │
    ▼ @mention 或 thread 訊息
┌─────────────────────────────────────────────────────────────────┐
│  openAB（Rust 主 process）                                       │
│                                                                 │
│  1. 收到 Discord 訊息                                            │
│  2. 找到或建立 agent session（per thread）                        │
│  3. spawn agent 時注入 env: OPENAB_THREAD_ID=<thread_id>         │
│  4. 送 prompt 給 agent                                           │
└──────────────────────┬──────────────────────────────────────────┘
                       │ ACP JSON-RPC (stdio)
                       ▼
┌─────────────────────────────────────────────────────────────────┐
│  AI Agent（kiro-cli / claude / codex / gemini）                   │
│                                                                 │
│  5. 從 SKILL.md / AGENTS.md 知道有 openab-cron 工具               │
│  6. 辨識排程意圖                                                  │
│  7. 呼叫 shell tool 執行 CLI：                                    │
│     openab-cron add --interval 1h --prompt "check weather"       │
│     （不需要帶 --thread，CLI 自動讀 env OPENAB_THREAD_ID）         │
└──────────────────────┬──────────────────────────────────────────┘
                       │ shell exec（繼承 env）
                       ▼
┌─────────────────────────────────────────────────────────────────┐
│  openab-cron CLI                                                 │
│                                                                 │
│  8. 讀取 OPENAB_THREAD_ID env var                                │
│  9. 寫入 cron_jobs.json（atomic write: temp + rename）            │
└──────────────────────┬──────────────────────────────────────────┘
                       │ JSON file
                       ▼
┌─────────────────────────────────────────────────────────────────┐
│  CronManager（openAB 主 process 內）                              │
│                                                                 │
│  10. 每 30 秒 reload JSON，同步 in-memory 任務                    │
│  11. 時間到 → spawn 臨時 ACP 連線（不佔 session pool）             │
│  12. 送 prompt → 收回應 → 發到 Discord thread                     │
│  13. 一次性 job → 執行後自動移除                                   │
└─────────────────────────────────────────────────────────────────┘
```

### 關鍵設計

- **工具暴露方式**：SKILL.md（Kiro）+ AGENTS.md（Claude/Codex/Gemini），agent 啟動時載入，每則訊息 0 額外 token
- **投遞目標**：環境變數 `OPENAB_THREAD_ID`，每個 thread 有獨立的 agent process，spawn 時注入，CLI 自動讀取，AI 不需要知道
- **排程類型**：`--interval`（固定間隔）、`--once`（一次性延遲）
- **執行模式**：獨立 session（每次 spawn 臨時 ACP 連線，不佔 pool，不干擾使用者對話）
- **持久化**：JSON 檔案，atomic write（temp + rename）

---

## 對照比較

### 工具暴露方式

| | OpenClaw | openAB |
|---|---------|--------|
| 方式 | 內建 tool calling → runtime 攔截 → Gateway WS | CLI + SKILL.md / AGENTS.md |
| AI 輸入格式 | JSON 物件 | Shell command + flags |
| 輸入修復 | ✅ normalize 層（欄位搬移、型別轉換） | ❌ CLI 嚴格解析（但 flag 本身自描述，不易出錯） |
| Token 成本 | Tool schema 在 session 建立時載入一次 | SKILL/AGENTS 在 agent 啟動時載入，0 per-message |
| 跨 agent 支援 | 單一 agent 架構 | ✅ 任何有 shell tool 的 agent 都能用 |

### 投遞目標解析

| | OpenClaw | openAB |
|---|---------|--------|
| 方式 | Session-based 自動推斷（session store） | 環境變數 `OPENAB_THREAD_ID` |
| AI 是否需要帶 ID | ❌ 完全不用 | ❌ 完全不用 |
| 複雜度 | 高（session store + fallback chain） | 低（env var，一行 code） |
| 跨 channel 投遞 | ✅ 支援 | ❌ 固定回到原 thread |

### 排程能力

| | OpenClaw | openAB |
|---|---------|--------|
| 固定間隔 | ✅ `everyMs` | ✅ `--interval` |
| 一次性延遲 | ✅ `at`（相對時間） | ✅ `--once --interval` |
| Cron 表達式 | ✅ `expr` + `tz` | 🔜 計劃支援（`--cron "0 9 * * *"`） |
| 絕對時間 | ✅ `at`（ISO-8601） | 🔜 計劃支援（`--at "2026-04-14T14:00"`） |
| 時區 | ✅ 每個 job 可設 | 🔜 計劃支援（`--tz "Asia/Taipei"`） |

### 執行模式

| | OpenClaw | openAB |
|---|---------|--------|
| AI 獨立 session | ✅ `agentTurn` | ✅ 臨時 ACP 連線 |
| AI 主對話注入 | ✅ `systemEvent` | ❓ 待討論（見注意事項） |
| 直接 script | ✅ `script` | ❓ 待討論 |
| 不佔 session pool | ✅ | ✅ |

**主對話注入的注意事項**：openAB 的 session 是 in-memory 的 agent process，與 OpenClaw 的持久化 session store 不同。主對話注入在 openAB 面臨額外挑戰：
- Session TTL 過期 → agent process 已被清掉，無法注入
- Session pool 滿 → 無法重建 session
- Agent 正在處理使用者訊息 → cron 注入與使用者對話衝突

### 可靠性

| | OpenClaw | openAB |
|---|---------|--------|
| 指數退避重試 | ✅ | ❓ 待討論 |
| 超時保護 | ✅ per-job | ❓ 待討論 |
| 失敗通知 | ✅ `failureAlert` | ❓ 待討論 |
| 重啟補跑 | ✅ 節流式 | ❓ 待討論 |
| 並行上限 | ✅ `maxConcurrentRuns` | ❓ 待討論 |
| Run log | ✅ `<jobId>.jsonl` | ❓ 待討論 |

### 安全性

| | OpenClaw | openAB |
|---|---------|--------|
| 限制誰能建立 | ✅ `ownerOnly` | ❓ 待討論（沿用 `allowed_users`？） |
| Per-job tool 限制 | ✅ `toolsAllow` | ❌ 繼承 agent 全域設定 |

---

## 已完成的 POC 實作

以下功能已有可運作的原型程式碼（尚未合併至 main）：

### 基礎架構
- **`openab-cron` CLI binary** — `add`、`list`、`remove` 子命令，用 clap 解析
- **`CronManager`** — openAB 主 process 內的排程管理器，每 30 秒 reload JSON，同步 in-memory 任務
- **JSON 持久化** — `cron_jobs.json`，atomic write（temp + rename）防損壞
- **`[cron]` config section** — `enabled`、`data_file` 設定，可透過 Helm values 控制

### 排程類型
- **固定間隔** — `openab-cron add --interval 1h --prompt "..."`
- **一次性延遲** — `openab-cron add --once --interval 10m --prompt "..."`，執行後自動移除

### 執行
- **獨立 session 執行** — CronManager 每次 spawn 臨時 AcpConnection，不佔 session pool，不受 `max_sessions` 限制
- **Thread 刪除偵測** — Discord thread 被刪時自動移除對應 job

### 工具暴露
- **SKILL.md** — Kiro CLI 用，放在 `~/.kiro/skills/openab-cron/SKILL.md`
- **AGENTS.md** — Claude Code / Codex / Gemini 用，放在 working directory
- **Dockerfile** — 啟動時從 `/opt/openab-skills/` 複製到 PVC mount 的 home 目錄

### Thread ID 傳遞
- **環境變數注入** — `pool.rs` 在 spawn agent 時注入 `OPENAB_THREAD_ID`
- **CLI 自動讀取** — `--thread` 為 optional，未指定時從 env var 讀取
- **AI 完全不用管** — 不需要 prompt injection，0 extra tokens per message

---

## 替代方案分析（Alternatives Considered）

### 1. Prompt injection vs SKILL.md

| | Prompt injection | SKILL.md |
|---|-----------------|----------|
| Token 成本 | ~150 tokens/msg | 0（agent 啟動時載入） |
| Thread ID | 直接寫在 prompt 裡 | 透過 env var，AI 不用管 |
| 維護性 | 改 Rust code | 改 markdown 檔案 |

選擇 SKILL.md：省 token、解耦、跨 agent 通用。

### 2. MCP server vs CLI

| | MCP server | CLI |
|---|-----------|-----|
| Token 成本 | ~300-500 tokens/session | 0（SKILL 描述） |
| 輸入格式 | Structured JSON | Shell flags |
| 跨 agent | 需要每個 agent 支援 MCP | ✅ 任何有 shell tool 的 agent |
| 開發成本 | 高（要寫 MCP server） | 低（clap CLI） |

選擇 CLI：更簡單、更通用、token 成本更低。

### 3. 環境變數 vs Session-based resolution

| | 環境變數 | Session-based（OpenClaw） |
|---|---------|--------------------------|
| 複雜度 | 低（spawn 時注入） | 高（session store + fallback chain） |
| 單一 channel | ✅ `OPENAB_THREAD_ID` | ✅ |
| 多 channel（同 pod） | ✅ 加 `OPENAB_SOURCE` 即可 | ✅ |
| 跨 channel session（同一使用者換 channel） | ❌ | ✅ |
| 適用場景 | 每個 thread/chat 一個 process（openAB） | 一個 session 可能跨 channel（OpenClaw） |

選擇環境變數：openAB 是 per-thread process，來源固定不會跨 channel。即使未來加入 Telegram 等，只需多注入 `OPENAB_SOURCE` env var，不需要 session store。

### 4. Gateway API vs JSON 檔案

| | Gateway API | JSON 檔案 |
|---|-----------|-----------|
| 狀態一致性 | ✅ 中央控制 | 需要 atomic write |
| 多 pod 共享 | ✅ | ❌ |
| 複雜度 | 高（多一個 process） | 低 |
| 適用場景 | 多 channel、多 pod | 單一 pod |

選擇 JSON 檔案：openAB 目前是單一 pod，不需要 Gateway 的複雜度。

---

## 未來可擴展功能（Future Possibilities）

### 排程增強
- **Cron 表達式** — `--cron "0 9 * * *"` 支援精確時間排程
- **絕對時間** — `--at "2026-04-14T14:00"` 指定確切執行時間
- **時區** — `--tz "Asia/Taipei"` 每個 job 可設時區

### 執行模式
- **主對話注入** — 在使用者現有 session 中注入（需解決 TTL、pool 上限、對話衝突問題）
- **直接 script** — 不經過 AI，直接跑 shell command（快、便宜、確定性高）

### 可靠性
- **指數退避重試** — 失敗後延後重試，成功歸零
- **超時保護** — 每個 job 可設 `timeoutSeconds`
- **失敗通知** — 連續失敗 N 次後通知使用者
- **重啟補跑** — Pod 重啟後節流式補跑錯過的 job
- **並行控制** — `maxConcurrentRuns` 限制同時執行數量

### 可觀測性
- **Run log** — 每次執行記錄（status、duration、token usage）
- **Token 用量追蹤** — 統計 cron job 的 AI 成本

### 多來源
- **Telegram / Slack 等** — job 資料模型預留 `source` 欄位
- **跨 channel 投遞** — 結果送到不同的 channel/thread

### 安全性
- **使用者限制** — 限制誰能建立 cron job
- **Per-job tool 限制** — 每個 job 可設允許的 tool 清單

---

## 討論問題

1. **Cron 表達式** — 是否支援 `0 9 * * *`？能實現「每個工作日早上 9 點」，固定間隔做不到。需要加 `cron` crate 依賴。

2. **主對話模式** — Cron job 是否能注入使用者現有的 session（帶對話上下文）？對「提醒」類任務有用，但 openAB 的 in-memory session 有 TTL 過期、pool 上限、對話衝突等問題。

3. **失敗處理** — 重試邏輯要多複雜？從「只報錯」到「指數退避 + 失敗通知」都有可能。

4. **重啟補跑** — Pod 重啟後，錯過的 job 要補跑嗎？還是直接跳過？

5. **執行記錄** — 是否記錄每次執行（狀態、耗時、token 用量）？

6. **安全性** — 建立 cron job 是否要限制特定 Discord 使用者？

7. **跨 channel 投遞** — Job 結果是否能送到不同的 channel/thread？

8. **並行上限** — 是否限制同時執行的 cron job 數量？

9. **多來源支援** — 目前來源是 Discord，未來可能加入 Telegram 等。同一個 pod 內 CLI + JSON 可以共用，但 job 資料模型可能需要預留 `source`（discord/telegram）和對應的 channel ID，讓 CronManager 投遞時知道送去哪。

歡迎回饋與建議！
