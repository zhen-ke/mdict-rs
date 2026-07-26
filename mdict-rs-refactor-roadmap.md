# mdict-rs 重构路线图（按优先级，可落地执行）

## 0. 先说结论

如果目标是把 `mdict-rs` 从“好用的本地离线词典服务”推进到“更可维护、更可演进、交互更成熟的产品级架构”，推荐按下面顺序推进：

1. **P0：先补齐首屏 setup / index status 体验**
2. **P1：再把 query 领域结果和 HTML 展示解耦**
3. **P2：最后拆薄 `AppState`，让内部职责真正分层**

这个顺序不是拍脑袋，而是基于当前代码结构决定的：

- 当前最大用户痛点在 **首次使用和索引期不可见**，所以 UX 要先改。
- 当前最大演进阻力在 **query 层直接产 HTML**，所以第二步要先把领域层和展示层拆开。
- 当前最大维护风险在 **`AppState` 太重**，但它适合在前两步稳定后做，不适合一上来硬拆。

---

## 1. 当前上下文与边界

这部分是路线图成立的前提，不能跳过。

### 1.1 当前项目真实结构

核心代码触点：

- 入口与路由：[`src/main.rs`](src/main.rs)
- 全局运行态：[`src/app_state.rs`](src/app_state.rs)
- HTTP handlers：[`src/handlers/mod.rs`](src/handlers/mod.rs)
- 查询服务：[`src/query/service.rs`](src/query/service.rs)
- 聚合 HTML 渲染：[`src/query/presenter.rs`](src/query/presenter.rs)
- 单词典查询：[`src/query/specific.rs`](src/query/specific.rs)
- 数据访问与重写：[`src/query/repository.rs`](src/query/repository.rs)、[`src/query/rewrite.rs`](src/query/rewrite.rs)
- 索引：[`src/indexing/mod.rs`](src/indexing/mod.rs)
- 前端：[`resources/static/index.html`](resources/static/index.html)、[`resources/static/index.js`](resources/static/index.js)、[`resources/static/index.css`](resources/static/index.css)

### 1.2 当前系统最重要的事实

1. **这是本地优先、离线优先的系统**
   - `.mdx/.mdd` 是事实源。
   - SQLite 只是索引层，不是主存储。

2. **请求路径已经可用，不适合大破大立**
   - `/query` 目前直接返回 HTML。
   - `/api/dicts`、`/api/index/status` 已存在。
   - `/dict/{id}/entry/{word}`、`/dict/{id}/res/{path}` 已有稳定语义。

3. **前端是 jQuery 单脚本模式**
   - 目前没有必要立刻重写成 React/Vue/Svelte。
   - 但可以先做“模块化拆分”和“状态模型收敛”。

4. **同步查询 + `spawn_blocking` 现在是可接受的**
   - 对当前目标（本地单用户/轻量多用户）不是第一瓶颈。
   - 所以本路线图不把“全异步化”作为优先项。

### 1.3 本次重构的硬边界

以下内容建议 **先不动**，否则范围会失控：

- 不更换 Axum
- 不替换 SQLite 索引方案
- 不做前后端分离 SPA 重写
- 不重做 MDX/MDD parser
- 不调整已有 URL 兼容性（尤其 `/query` 和 legacy resource route）

### 1.4 重构期间必须保持的兼容性

#### 后端兼容
- `/query` 继续可用，且默认仍能返回 HTML，避免当前前端立刻失效
- `/api/dicts` 保持字段兼容，新增字段只增不删
- `/api/index/status` 保持已有语义
- `/dict/*` 与 `/resource/*` 路由不破坏

#### 前端兼容
- 旧的 hash 路由继续可用
- 历史记录 localStorage key 尽量不改
- 搜索、suggest、分享、lucky 功能持续可用

---

## 2. 目标架构：改到什么样算“更优解”

### 2.1 目标不是“更复杂”，而是“边界清晰”

建议演进到下面的分层：

```text
HTTP / UI Layer
├── handlers/
├── web api dto
└── html presenter

Application Layer
├── QueryService
├── IndexStatusService
├── SetupStatusService
└── DictionaryCatalogService

Domain / Query Layer
├── AggregateQueryResult
├── DictEntryResult
├── QueryTrace
└── query rules / redirect rules / candidate expansion

Infrastructure Layer
├── SqliteIndexRepository
├── MdxReaderRegistry
├── ResourceResolver
├── CacheStore
└── BlockingQueryLimiter
```

### 2.2 演进后的核心原则

1. **Query 先产“结构化结果”，再决定渲染成 HTML 还是 JSON**
2. **状态信息（setup/indexing/partial-ready）必须是首屏一等公民**
3. **`AppState` 只做组合根，不再做万能对象**
4. **每一步都可以单独发布，不依赖一次性大重构**

---

## 3. 优先级路线图

---

# Phase 0（准备阶段，0.5 ~ 1 天）

> 目标：先把“后续重构不会误伤”的护栏补上。

## 3.1 要做什么

### Task 0.1：补一个轻量基线文档
- 记录当前公开接口：
  - `POST /query`
  - `GET /suggest`
  - `GET /api/dicts`
  - `GET /api/index/status`
  - `GET /dict/{id}/entry/{word}`
  - `GET /dict/{id}/res/{path}`
  - `GET /dict/{id}/audio/{path}`
- 说明返回类型、前端依赖点、缓存行为。

### Task 0.2：补 3 类回归测试/快照测试
- `/query` 返回 HTML 的基本行为
- `/api/dicts` 返回结构的稳定性
- `/api/index/status` 在“无词典 / 部分索引 / 已完成”场景下的行为

### Task 0.3：清理文案和事实不一致
- `index.html` 中 “Rust + Sled” 改为 “Rust + SQLite”
- README 中相关表述同步修正

## 3.2 为什么先做

因为后面 Phase 1 ~ 3 都会改核心路径。没有基线，很容易把“兼容性”说在嘴上，实际悄悄破坏掉。

## 3.3 触点文件

- [`src/main.rs`](src/main.rs)
- [`src/handlers/mod.rs`](src/handlers/mod.rs)
- [`resources/static/index.html`](resources/static/index.html)
- [`README.md`](README.md)

## 3.4 验收标准

- 至少有一组可重复执行的接口回归验证
- 文案不再出现 Sled 残留
- 不引入行为变更

---

# Phase 1（最高优先级，1 ~ 2 天）

> 目标：把“首次使用 / 索引中 / 无词典 / 部分可用”变成清晰可感知的产品体验。

## 4. 要解决的真实问题

当前问题不是“没有接口”，而是**接口能力没有被组织成用户能理解的状态**。

现在已有：
- `/api/dicts`
- `/api/index/status`

但前端首屏没有建立一套 setup/status 心智模型，导致：
- 没有词典时像坏了
- 在后台建索引时像搜不到
- 部分词典可用时用户不知道哪些可用

## 4.1 本阶段目标输出

新增一个“首屏状态模型”，前端加载时先判定：

- `no_dicts`
- `indexing_pending`
- `partially_ready`
- `ready`
- `status_unavailable`（接口失败，但服务仍可继续）

### 推荐做法

#### 方案 A（推荐）
新增一个聚合接口，例如：
- `GET /api/setup/status`

返回：

```json
{
  "dict_dir": "...",
  "dict_count": 2,
  "text_dict_count": 1,
  "resource_dict_count": 1,
  "ready_count": 1,
  "pending_count": 0,
  "stale_count": 0,
  "status": "ready",
  "dictionaries": [
    {
      "id": "...",
      "name": "...",
      "db_exists": true,
      "up_to_date": true,
      "fts_enabled": true,
      "has_fts": true
    }
  ]
}
```

优点：
- 前端不用自己拼状态
- 以后 setup 页面、诊断页都能复用

#### 方案 B（次优）
继续只用 `/api/dicts` + `/api/index/status`，但前端自行聚合。

缺点：
- 业务规则散落到前端
- 以后状态逻辑更难维护

## 4.2 可落地任务拆分

### Task 1.1：设计 setup status DTO
**产出**：统一状态模型

字段至少包括：
- 词典目录路径
- 总词典数 / text 词典数 / resource 词典数
- ready / pending / stale 数量
- overall status
- 每本词典的索引状态
- 是否存在配置错误/读取失败

**边界要考虑**：
- 目录不存在
- 目录存在但为空
- 只有 `.mdd` 没有 `.mdx`
- 有 `.mdx` 但 `.db` 不存在
- `.db` 存在但已过期
- `index_status(file)` 读取失败

### Task 1.2：后端实现 setup/status 聚合接口
**建议新建服务**：`SetupStatusService`

职责：
- 从 `AppState`/catalog 拿到词典列表
- 从 `indexing::index_status()` 聚合索引状态
- 输出前端可直接消费的数据

**建议代码落点**：
- `src/handlers/mod.rs`：新增 handler
- `src/app_state.rs`：如果不想立刻拆，可先新增只读辅助方法
- 后续可迁到单独模块，如 `src/status/` 或 `src/services/setup_status.rs`

### Task 1.3：前端首屏接入 setup/status
**前端行为建议**：
- `document.ready` 时先请求 setup status
- 根据状态渲染不同首屏：
  - `no_dicts`：显示引导
  - `indexing_pending`：显示“正在建立索引，可稍后重试”
  - `partially_ready`：显示“部分词典已可用” + 可搜索
  - `ready`：正常欢迎页
  - `status_unavailable`：降级到现有欢迎页

### Task 1.4：把状态展示做成独立 UI block
不要把状态提示直接散落在 `showWelcome()` / `showEmpty()` 里。

建议抽出：
- `renderSetupState(status)`
- `renderReadyWelcome(status)`
- `renderIndexingState(status)`

### Task 1.5：对查询错误做状态化映射
如果用户查询失败，前端要能区分：
- 未找到词条
- 服务过载（429/503 语义）
- 索引未准备好
- 一般内部错误

即便后端暂时不新增错误码，也要先给前端一层清晰映射。

## 4.3 触点文件

- [`src/handlers/mod.rs`](src/handlers/mod.rs)
- [`src/main.rs`](src/main.rs)
- [`src/app_state.rs`](src/app_state.rs)
- [`src/indexing/mod.rs`](src/indexing/mod.rs)
- [`resources/static/index.js`](resources/static/index.js)
- [`resources/static/index.html`](resources/static/index.html)
- [`resources/static/index.css`](resources/static/index.css)

## 4.4 风险与规避

### 风险 1：状态判断重复
如果前端和后端都在拼 setup 状态，未来必然分叉。

**规避**：状态计算尽量只在后端做一次。

### 风险 2：索引是后台异步，状态会瞬时变化
用户打开页面时可能是 `pending`，几秒后变 `ready`。

**规避**：
- 首屏加载一次
- 如处于 `pending`，可每 3~5 秒轮询一次 `/api/setup/status`
- 轮询仅在 pending 时开启

### 风险 3：无词典但服务仍正常
不要把“无词典”当成 500 错误。

**规避**：把它当正常 product state。

## 4.5 验收标准

- 无词典时首页能明确告诉用户怎么配置
- 索引中时首页能展示“可等待”的状态，不再像坏掉
- 部分可用时用户能知道哪些词典可用
- 不影响现有搜索、suggest、share、lucky 流程

---

# Phase 2（第二优先级，2 ~ 4 天）

> 目标：让 query 层先产结构化结果，再由 presenter 决定怎么输出 HTML。

## 5. 当前问题在哪里

当前 [`src/query/service.rs`](src/query/service.rs) 中：
- 聚合查询完成后，直接组装 `AggregateSection`
- 调用 [`src/query/presenter.rs`](src/query/presenter.rs) 直接生成 HTML
- handler 最终返回 HTML response

问题是：
- query service 同时承担应用编排 + 视图输出责任
- 将来做 JSON API、更多筛选、UI 层局部渲染时会很别扭

## 5.1 本阶段目标输出

把查询结果拆成两层：

### 领域结果层
例如：

```rust
pub struct AggregateQueryResult {
    pub query: String,
    pub sections: Vec<DictionaryEntryResult>,
    pub status: AggregateQueryStatus,
}

pub struct DictionaryEntryResult {
    pub dict_id: String,
    pub dict_name: String,
    pub container_class: Option<String>,
    pub body_html: String,
}
```

### 展示层
- `render_aggregate_html(&AggregateQueryResult) -> String`
- 后续可加：`impl Serialize for AggregateQueryResult` 或 API DTO

## 5.2 可落地任务拆分

### Task 2.1：新增 query result model
建议新增文件：
- `src/query/model.rs`

先定义：
- `AggregateQueryResult`
- `DictionaryEntryResult`
- `AggregateQueryStatus`
- `QueryTraceResult`（可顺手把 trace 也统一）

### Task 2.2：把 `query_aggregate_entries` 改为返回结构化结果
当前：
- 返回 `(Bytes, String)`

目标：
- 先内部返回 `AggregateQueryResult`
- 在兼容层再转换成 `(Bytes, String)`

建议过渡方式：

```rust
pub fn query_aggregate_result(...) -> Result<AggregateQueryResult, QueryError>
pub fn query_aggregate_html(...) -> Result<(Bytes, String), QueryError>
```

然后：
- `handle_query` 先继续调用 HTML 版本
- 新 API 可以以后再接结构化版本

### Task 2.3：presenter 改成消费 result model
把 [`src/query/presenter.rs`](src/query/presenter.rs) 从“吃 section”改成“吃完整 result”。

这样以后 presenter 可以：
- 根据 `status` 渲染空态
- 根据 section 元信息渲染筛选 UI
- 不必从零散参数拼装

### Task 2.4：将 handler 层和 query/service 的 response glue 明确分离
现在 handler 关心：
- 缓存
- blocking 调度
- HTTP response
- query 调用

建议在这一阶段至少收敛成：
- query service：只返回业务结果
- handler：负责 HTTP 包装
- presenter：负责 HTML 字符串

### Task 2.5：新增 JSON 查询接口（可选，但强烈建议）
在保留 `/query` HTML 的前提下，新增例如：
- `GET /api/query?q=word`

返回结构化 `AggregateQueryResult`。

这一步不是为了“马上换前端”，而是为了验证架构真的解耦了。

## 5.3 关键边界

### 边界 1：资源查询不要被误抽象成 entry query
`query()` 目前既能查 entry，也能查 resource key。

建议保留分流：
- entry domain result
- resource lookup 不强行塞到同一返回模型

### 边界 2：HTML body 仍然是已 rewrite 的 HTML
这里不要追求一步到位改成纯结构化词典 AST。

当前最现实的分层是：
- 领域结果里仍携带 `body_html`
- 但页面级 HTML（卡片壳子、标题、布局）由 presenter 负责

### 边界 3：错误语义不要丢
`QueryError::NotFound / TooManyRedirects / InvalidInput / Internal` 要保留，并在 handler 继续映射到 HTTP。

## 5.4 风险与规避

### 风险 1：一次改太大导致 handler 全断
**规避**：先加新函数，不要立刻替换旧函数签名。

### 风险 2：缓存 key 与缓存内容模型变化
现在 entry cache 缓的是最终 HTML payload。

**建议过渡策略**：
- 先维持缓存 HTML，不马上缓存结构化结果
- 等模型稳定后，再决定是否缓存 result model

### 风险 3：specific query 与 aggregate query 重复模型
**规避**：先只给 aggregate query 建 model；单词典结果可以在第二步复用或适配。

## 5.5 触点文件

- [`src/query/mod.rs`](src/query/mod.rs)
- [`src/query/service.rs`](src/query/service.rs)
- [`src/query/presenter.rs`](src/query/presenter.rs)
- [`src/query/specific.rs`](src/query/specific.rs)
- [`src/handlers/mod.rs`](src/handlers/mod.rs)

## 5.6 验收标准

- `/query` 仍保持现有可见行为
- query service 内部不再直接依赖页面级渲染细节
- 新增结构化 query result model
- 至少能额外暴露一个 JSON 查询接口，证明解耦成立

---

# Phase 3（第三优先级，3 ~ 5 天）

> 目标：把 `AppState` 从“万能对象”降级为“组合根 + facade”。

## 6. 当前问题在哪里

[`src/app_state.rs`](src/app_state.rs) 已经开始有内部分层雏形：
- `DictCatalog`
- `RuntimeState`

这是好事，说明已经有拆分基础。

但现在问题仍在：
- catalog 相关逻辑、cache、reader registry、db pool、dict config 查询都还挂在 `AppState` 方法上
- 外部代码几乎把 `AppState` 当服务定位器使用

## 6.1 本阶段目标输出

目标不是“删除 AppState”，而是让它只做：
- 组合依赖
- 提供少量稳定 facade
- 让内部服务可独立演进

推荐拆成：

### 6.1.1 `catalog` 模块
职责：
- 词典发现结果
- dict id 分配
- id/path 映射
- dict info / display name / config 查询

### 6.1.2 `runtime` 模块
职责：
- db pool registry
- mdx reader registry
- blocking query limiter

### 6.1.3 `cache` 模块
职责：
- entry cache
- resource cache
- negative cache
- TTL/LRU 策略

### 6.1.4 `status` 或 `services` 模块
职责：
- setup status 聚合
- index status 聚合

## 6.2 可落地任务拆分

### Task 3.1：先做文件级拆分，不先改外部接口
建议先把 `app_state.rs` 拆为：

- `src/app_state/mod.rs`
- `src/app_state/catalog.rs`
- `src/app_state/runtime.rs`
- `src/app_state/cache.rs`

但 `pub struct AppState` 可以暂时保留原样。

这是风险最低的一步。

### Task 3.2：把 catalog 访问改为显式 service
例如从：
- `state.get_dict_id(file)`
- `state.get_all_dict_info()`

逐步变成：
- `state.catalog().dict_id_for(file)`
- `state.catalog().all_dict_info()`

如果不想暴露内部对象，也可以先在内部用私有方法转发。

### Task 3.3：把 cache 访问从业务语义抽离
现在 handler 里写的是：
- `state.get_entry_cached(...)`
- `state.put_negative_cache(...)`

建议改成语义更明确的 cache facade：
- `state.entry_cache().get_html(...)`
- `state.resource_cache().put(...)`
- `state.negative_cache().contains(...)`

即便最后仍由 `AppState` 转发，职责也会更清晰。

### Task 3.4：把 db / reader registry 独立成 runtime service
目前：
- `state.get_db_connection(file)`
- `state.get_mdx_reader(file)`

建议收敛为：
- `state.runtime().db().connection_for(file)`
- `state.runtime().readers().reader_for(file)`

### Task 3.5：清掉 `AppState` 中非 facade 级工具函数
比如：
- `is_mdx_file`
- `is_mdd_file`
- `logical_dict_key`
- `allocate_dict_id`

应该移动到更贴近 `catalog` 的模块内。

## 6.3 关键边界

### 边界 1：不要在这一步同时改 handler 语义
这一步目标是内部可维护性，不是外部行为升级。

### 边界 2：不要在这一步引入过多 trait 抽象
项目还不大，没必要为了“可测试”先上很多 trait object。

更好的节奏是：
- 先按模块拆清楚
- 真有替换需求时再抽 trait

### 边界 3：保持 `Clone + Send + Sync` 使用体验
`AppState` 目前作为 Axum state 使用，迁移时不能破坏这点。

## 6.4 风险与规避

### 风险 1：拆文件时引入大量 `pub(crate)` 泄漏
**规避**：优先私有模块 + 必要 re-export，不要急着把内部结构全公开。

### 风险 2：内部拆分后 handler 改动面太大
**规避**：保留旧 facade 方法一段时间，逐步迁移调用点。

### 风险 3：缓存和 runtime 并发语义被无意改变
**规避**：拆分时先不改锁模型、不改 TTL、不改容量。

## 6.5 触点文件

- [`src/app_state.rs`](src/app_state.rs) → 拆分为子模块
- [`src/handlers/mod.rs`](src/handlers/mod.rs)
- [`src/query/service.rs`](src/query/service.rs)
- [`src/query/specific.rs`](src/query/specific.rs)
- [`src/query/repository.rs`](src/query/repository.rs)

## 6.6 验收标准

- `AppState` 文件长度和职责显著下降
- catalog/runtime/cache/status 各自职责边界清晰
- 外部行为无变化
- 测试通过，且后续新增功能不再必须把逻辑继续堆进 `AppState`

---

# Phase 4（建议跟进，2 ~ 3 天）

> 目标：前端不重写框架，但要停止继续堆 monolithic `index.js`。

这个阶段不属于你之前点名的 3 个最高优先项，但它是完成前 3 步后的自然收口。

## 7.1 为什么建议做

如果只改后端，不改前端组织方式：
- setup/status 逻辑会继续塞进 `index.js`
- query model 解耦后，前端还是难维护

## 7.2 可落地任务拆分

### Task 4.1：把 `index.js` 按职责拆模块
建议拆为：
- `boot.js`：初始化与首屏状态
- `router.js`：URL/hash/history
- `query.js`：查询请求与响应处理
- `status.js`：setup/index status UI
- `suggest.js`：联想词
- `history.js`：本地历史
- `render.js`：empty/loading/error/result 渲染

如果暂时不引入打包器，也可以多 `<script>` 文件串联加载。

### Task 4.2：引入统一 UI state model
至少前端要有一个统一状态对象：

```js
{
  setupStatus,
  currentQuery,
  isLoading,
  error,
  suggestions,
  history
}
```

### Task 4.3：把 AJAX 请求加上 stale request 防护
尤其 suggest / query：
- 防止慢请求覆盖快请求
- 可逐步改成 `fetch + AbortController`

## 7.3 验收标准

- `index.js` 不再是单文件承载全部逻辑
- setup/status/query 的 UI 更新点可定位
- 后续加“词典筛选 / 置顶 / 折叠”不会进一步恶化结构

---

## 8. 推荐执行顺序（最实用版本）

如果你想最低风险推进，建议按下面顺序执行：

### 里程碑 M1：用户先感觉到明显变好
- Phase 0
- Phase 1

**结果**：
- 首次使用体验明显提升
- 无词典 / 索引中 / 部分可用状态都清楚
- 用户能理解系统在干什么

### 里程碑 M2：代码边界开始清晰
- Phase 2

**结果**：
- 后端查询层不再与 HTML 强耦合
- 后续做 JSON API、筛选 UI、局部刷新都更容易

### 里程碑 M3：内部维护成本下降
- Phase 3

**结果**：
- `AppState` 不再持续膨胀
- 新功能更容易找到归属

### 里程碑 M4：前端组织跟上后端演进
- Phase 4

**结果**：
- 前后端边界都更健康
- 项目进入“可持续迭代”状态

---

## 9. 我建议你下一轮直接开的任务单

下面是可以直接开始做的 backlog。

### P0 backlog
- [ ] 修正文案中的 `Sled` 残留
- [ ] 为 `/api/dicts`、`/api/index/status`、`/query` 建最小回归验证
- [ ] 设计 `/api/setup/status` DTO
- [ ] 实现 `/api/setup/status`
- [ ] 首页接 setup/status 首屏分流
- [ ] pending 状态增加轮询刷新
- [ ] 将“无词典 / 索引中 / 部分可用 / 已就绪”做成 4 种独立空态

### P1 backlog
- [ ] 新建 `src/query/model.rs`
- [ ] 抽出 `query_aggregate_result()`
- [ ] `presenter` 改为消费 result model
- [ ] 保持 `/query` HTML 兼容
- [ ] 增加 `/api/query` JSON 接口
- [ ] 为 query result 增加测试

### P2 backlog
- [ ] 将 `app_state.rs` 拆成 `mod.rs/catalog.rs/runtime.rs/cache.rs`
- [ ] 将 dict id / dict info / config 查询归档到 catalog
- [ ] 将 db pool / reader registry 归档到 runtime
- [ ] 将 entry/resource/negative cache 归档到 cache
- [ ] 让 `AppState` 保留 facade，逐步迁移调用点

### P3 backlog
- [ ] 拆 `index.js`
- [ ] 引入统一前端状态对象
- [ ] 给 query/suggest 加 stale-request 防护
- [ ] 为后续“词典筛选 / 置顶 / 折叠”预留 UI 状态位

---

## 10. 每阶段完成后的“不要急着做”

为了避免范围失控，每个阶段完成后，这些事都建议先别做：

### Phase 1 后不要急着做
- 不要立刻重写前端框架
- 不要顺手改全部错误处理协议

### Phase 2 后不要急着做
- 不要马上把所有 HTML body 再抽成 AST
- 不要马上重写 resource query 模型

### Phase 3 后不要急着做
- 不要为了“优雅”上过多 trait/DI 抽象
- 不要引入复杂 service locator 或 container

---

## 11. 最终判断：什么叫“改到更优解”

对这个项目来说，“更优解”不是：
- 更大
- 更潮
- 更像前后端分离大项目

而是：

1. **用户在任何状态下都知道系统当前是否可用、为什么不可用**
2. **query 结果能脱离 HTML 页面结构而独立存在**
3. **`AppState` 不再成为一切功能的默认落点**
4. **新增功能时，开发者能很自然地知道该改哪一层**

如果这 4 点做到，这个项目就会从“做得不错的工具”升级成“结构健康、能持续迭代的产品底座”。

---

## 12. 推荐的首个执行批次（我认为最划算）

如果你只想先做一个小版本，我建议直接做下面这一批：

### Batch 1
- 修正文案/README 的 SQLite 表述
- 新增 `/api/setup/status`
- 首页接入 setup/status
- pending 状态轮询
- 无词典 / 索引中 / 部分可用 / ready 四态首屏

这是 **投入最小、用户感知最大** 的一批。

### Batch 2
- 新建 `query/model.rs`
- 抽 `query_aggregate_result()`
- presenter 改吃 result model
- 保持 `/query` 兼容
- 新增 `/api/query`

这是 **架构收益最大** 的一批。

### Batch 3
- 拆 `app_state.rs`
- catalog/runtime/cache 模块化
- 保持 facade 兼容

这是 **长期维护收益最大** 的一批。
