# Bone TUI 原生 OpenTUI 重构执行计划

## 1. 任务目标

将 Bone 当前“使用 OpenTUI 作为底层，但在其上重新实现一套 TUI 框架”的结构，重构为直接使用 `@opentui/core` 原生 renderable、renderer、focus、layout、input、scroll 和 testing 能力。

本次重构的核心目标不是修复某一个输入框焦点问题，而是系统性降低 TUI 部分的代码量、状态数量和生命周期复杂度，避免继续在 Bone 层复制 OpenTUI 已经拥有的能力。

目标结果：

- TUI 代码更少、更直接；
- 只有一套 native focus 真相；
- 只有一套 renderable 生命周期；
- 只有一个 overlay/dialog 生命周期管理器；
- 普通输入、选择、滚动尽可能交给 OpenTUI 原生控件；
- Bone 代码只保留产品语义；
- Extension UI V2 保持为产品 contract，但内部实现直接使用 OpenTUI；
- 性能优化以原地更新 native renderable 为基础，不再依赖大范围 unbind/rebind；
- 删除 Bone node、layout、event、renderer 和 test renderer 的平行抽象。

## 2. 当前上下文和暂停点

### 2.1 用户已经确认的方向

- 产品运行时只使用 Bun；不需要保留 Node 运行时兼容。
- 可以改变内部产品 contract，不要求为旧的 Bone TUI 内部 API 保持兼容。
- UI 应接近 Codex、Claude Code、OpenCode 的交互密度和布局逻辑。
- sidebar 用于会话切换；主区域用于 transcript 和 composer。
- 不做可配置 keybinding。
- 当前输入框 slash command 退出后的焦点问题已经暂停，不继续在旧架构上叠加补丁。
- 这次必须做架构级修复，并使用多 agent 并行实现和独立 agent review。

### 2.2 最近暴露的结构性问题

slash command dialog 退出后曾出现：

1. composer 暂时无法继续输入；
2. 恢复输入后布局错位；
3. dialog 子 input/select 获得 native focus，但 Bone pane controller 仍认为 composer 活跃；
4. modal 的 Esc、Enter、方向键可能同时被后台 composer/sidebar 和 dialog 处理；
5. theme/images 刷新会解绑并重建整个 foreground，和 dialog close、focus restore、layout reflow 交错。

已经确认存在两套焦点状态：

```text
OpenTUI CliRenderer.currentFocusedRenderable
OpenTUIPaneFocusController.activeId
```

已经确认当前 overlay 生命周期分散在：

- `packages/tui/src/opentui/renderer.ts`
- `packages/coding-agent/src/modes/interactive/opentui-extension-host.ts`
- selector/input/form/login view 的 `mount()` / `focus()`
- `OpenTUIPaneFocusController`
- `OpenTUITranscriptFocusController`
- `OpenTUIInteractiveMode.refreshPresentation()`

### 2.3 旧焦点修复的处理原则

主线程开始重构前必须先执行只读审计：

```bash
git status --short
git diff --stat
git diff -- packages/tui/src/opentui \
  packages/coding-agent/src/modes/interactive
```

不要直接 reset、checkout、stash 或整体回滚。当前工作树可能同时包含：

- 旧焦点修复；
- 输入输出流隔离；
- sidebar/UI 调整；
- 其他 agent 或用户的未提交工作。

应逐文件标记：

```text
A. 新架构仍需要，直接吸收
B. 旧架构过渡补丁，待 native replacement 完成后删除
C. 与本重构无关，保留且不修改
D. 无法判断，先向用户确认
```

严禁为了获得“干净工作树”而破坏其他工作。

## 3. 当前复杂度基线

当前 `packages/tui/src/opentui` 约 1,756 行：

| 文件 | 大致行数 | 当前职责 |
| --- | ---: | --- |
| `types.ts` | 510 | 复制 OpenTUI node、layout、event、renderer 类型 |
| `nodes.ts` | 789 | facade、属性转发、事件转换、native node 映射 |
| `renderer.ts` | 374 | renderer、node factory、focus、overlay、key broadcast |
| `testing.ts` | 83 | Bone TestRenderer wrapper |

interactive OpenTUI 相关代码约 8,900 行，其中核心 shell、mode、extension host、focus controller 超过 4,000 行。

当前结构：

```text
OpenTUI Renderable
  -> BoneNode facade
    -> BoneRenderer facade
      -> PaneFocusController
        -> component-local focus/key/layout state
```

目标结构：

```text
@opentui/core native renderables
  -> Bone product components
    -> session/transcript/sidebar/composer product state
```

## 4. 不变的产品能力

重构不是功能删减。以下能力必须保留：

- 多会话 sidebar；
- 当前会话、选中会话、运行中会话的视觉状态；
- 后台会话实时输出预览和吞吐；
- 每个会话独立的输入输出流；
- 切换会话后恢复 draft 和 transcript scroll；
- 当前会话流式输出；
- slash commands；
- extension dialogs、widgets、chrome、custom editor view、tool result renderer；
- native textarea 输入、粘贴、提交、取消；
- transcript mouse/keyboard scroll 和 sticky follow；
- theme、inline images、thinking display；
- resize；
- tmux/真实终端行为；
- Bun package 和 standalone binary。

不能借重构之名删除看起来“暂时难迁”的能力。确实需要删除或改变时，必须先向用户说明产品影响。

## 5. 目标架构

### 5.1 直接使用的 OpenTUI 原生类型

组件直接依赖：

```ts
import {
  BoxRenderable,
  CliRenderer,
  DiffRenderable,
  InputRenderable,
  MarkdownRenderable,
  Renderable,
  ScrollBoxRenderable,
  SelectRenderable,
  TextareaRenderable,
  TextRenderable,
} from "@opentui/core";
```

不再定义或使用：

```text
BoneNode
BoneContainerNode
BoneTextNode
BoneInputNode
BoneTextareaNode
BoneScrollViewNode
BoneSelectNode
BoneRenderContext
BoneView
BoneRenderer
BoneTestRenderer
getNativeNode
BoneNodeFactory
```

### 5.2 `packages/tui` 的最终职责

`packages/tui` 最终只能保留 Bone 真正需要的薄层：

```text
packages/tui/src/
  renderer.ts          # createCliRenderer 的产品默认配置
  overlay-manager.ts   # 产品需要的 modal/overlay 生命周期
  index.ts             # 明确导出，不做 broad barrel
```

如果最终 `renderer.ts` 只剩几十行默认配置，应评估是否彻底删除独立 `@frelion/bone-tui` 包，由 coding-agent 直接依赖 `@opentui/core`。不要为了保留包而创造新的抽象。

### 5.3 组件 contract

组件不再实现 `BoneView.mount()`，而是持有 native renderable：

```ts
interface ComposerView {
  readonly root: BoxRenderable;
  readonly input: TextareaRenderable;
  focus(): void;
  destroy(): void;
}
```

静态或一次性 view 可以使用 factory：

```ts
type RenderableFactory<T extends Renderable = Renderable> =
  (renderer: CliRenderer) => T;
```

factory 只能创建 tree，不允许在 tree attach 之前隐式 focus。

### 5.4 单一焦点真相

native focus 只由 OpenTUI 拥有：

```text
CliRenderer.currentFocusedRenderable
```

Bone 可以保留 semantic pane：

```ts
type PaneId = "sidebar" | "transcript" | "composer";
```

但 semantic pane 只能通过统一入口改变：

```ts
focusPane(id)
  -> native renderable.focus()
  -> update semantic pane
  -> update visual state
  -> requestRender()
```

禁止 pane state 和 native focus 分别由不同模块修改。

### 5.5 单一 overlay 生命周期

所有 select、confirm、input、advanced view、login、settings dialog 都必须进入同一个 `OpenTUIOverlayManager`。

打开顺序：

```text
capture explicit application focus target
create native renderable tree
attach overlay wrapper to renderer root
apply native layout
focus explicit dialog control
activate modal action routing
```

关闭顺序：

```text
mark closing / guarantee idempotence
deactivate modal action routing
blur currently focused overlay descendant
destroy overlay subtree
remove/hide empty overlay layer
focus explicit application target
update semantic pane
requestRender
resolve dialog promise
```

必须使用明确的状态机：

```ts
type OverlayState =
  | { kind: "idle" }
  | { kind: "opening"; id: number }
  | { kind: "open"; id: number; root: Renderable }
  | { kind: "closing"; id: number };
```

abort、timeout、Esc、confirm、dispose、factory reject 必须汇聚到同一个幂等 close 入口。

### 5.6 原生键盘事件

普通控件行为交给 OpenTUI：

- textarea 文字输入；
- cursor movement；
- delete/backspace；
- paste；
- input submit/cancel；
- select navigation；
- ScrollBox scrolling。

Bone 只处理应用级动作：

- pane navigation；
- interrupt；
- quit；
- conversation navigation；
- slash command product actions。

禁止把每个 native key event 转成 `BoneKeyEvent` 后广播给所有 pane。

### 5.7 原生 scroll

`ScrollBoxRenderable` 是 scrollTop、viewport、scrollbar、sticky follow 和 mouse wheel 的唯一所有者。

`OpenTUITranscriptFocusController` 应拆分为：

```text
TranscriptScrollState
  - autoFollowing
  - followLatest()
  - syncAutoFollow()

PaneNavigator
  - focus sidebar/transcript/composer

AppShortcutRouter
  - interrupt/quit/application actions
```

不能再额外创建一个 scroll wrapper 或手工同步两套 viewport。

### 5.8 Extension UI V2

Extension UI V2 是产品 contract，应保留。其 OpenTUI implementation 应直接使用 native renderable。

需要保留：

```text
dialogs.select
dialogs.confirm
dialogs.input
widgets.set/clear
chrome.setHeader/setFooter/setTitle
editor.open/setView
toolResults.setRenderer
advanced.show/close
```

需要删除：

- Extension contract 对 `BoneView` 的依赖；
- `MountedSlot` 对 Bone mount/unmount 的依赖；
- dialog view 在 `mount()` 内抢焦点；
- `runDialog()` 和 `showAdvanced()` 各自维护 close 逻辑；
- extension host 自己猜测 previous focus。

## 6. 多 agent 执行组织

主线程应创建独立 worktree 或在明确的重构分支上执行。默认分支名建议：

```text
codex/opentui-native-refactor
```

如果当前 worktree 有未提交工作，必须先完成第 2.3 节的归属审计，不能直接移动、stash 或覆盖。

### Agent 0：主协调 agent

职责：

- 读取本计划完整内容；
- 审计当前 worktree；
- 建立 migration matrix；
- 确定 public/internal contract；
- 控制共享文件修改顺序；
- 合并各 agent 结果；
- 处理交叉冲突；
- 运行最终 check、tests、tmux smoke、性能基线；
- 不在各 agent 工作未完成时提前声明成功。

主 agent 独占文件建议：

```text
packages/coding-agent/src/modes/interactive/opentui-interactive-mode.ts
packages/coding-agent/src/modes/interactive/opentui-shell.ts
packages/tui/src/index.ts
package.json / lockfiles（如确实需要）
```

### Agent A：native TUI kernel

职责：

- 核对 `@opentui/core@0.4.5` 的真实类型和 runtime；
- 创建最小 renderer bootstrap；
- 创建 native overlay manager；
- 制定并实现 native renderable factory contract；
- 迁移或删除 `types.ts`、`nodes.ts`、`renderer.ts`、`testing.ts`；
- 使用 OpenTUI 官方 TestRenderer、mock keys、mock mouse；
- 不修改 coding-agent product behavior。

交付：

- kernel API；
- overlay lifecycle tests；
- native focus descendant restore tests；
- resize/hide/show/close tests；
- 删除代码统计。

### Agent B：shell、composer、sidebar、transcript

职责：

- 迁移 shell tree 到 native renderables；
- 迁移 composer；
- 迁移 session sidebar；
- 迁移 transcript ScrollBox；
- 拆分 pane navigation 和 transcript scroll state；
- 保留所有产品交互和视觉 contract；
- 不修改 extension host。

重点验收：

- composer 始终拥有明确 native focus；
- sidebar 搜索和 resize 正常；
- sidebar 宽度 mouse drag 正常；
- transcript 不存在重复 scroll owner；
- 切换会话 draft/scroll 状态恢复；
- 运行中后台会话 preview/throughput 正常。

### Agent C：dialogs 和 extension host

职责：

- 迁移 select/confirm/input/form/login/settings；
- 迁移 Extension UI V2 OpenTUI implementation；
- 所有 dialog 使用 Agent A 的 overlay manager；
- 删除 mount 阶段隐式 focus；
- 合并 `runDialog()` 和 `showAdvanced()` 的 lifecycle；
- 处理 abort/timeout/dispose/factory race；
- 保证 modal 不触发后台 composer/sidebar action。

重点验收：

- `/settings` Esc 后立即输入；
- selector/input/form/login 一致的 focus contract；
- 连续打开/关闭 10 次无 destroyed focus；
- async advanced factory 取消后不 attach；
- close 只 resolve 一次；
- modal Enter/Esc/arrows 不泄漏；
- modal 普通文字仍进入 native input。

### Agent D：独立 review agent

必须在实现完成后创建，不能承担实现工作。review 范围：

- 是否真的删除平行抽象，而不是换名字；
- 是否仍有两套 focus state；
- 是否仍有 mount-before-attach；
- 是否仍有 key broadcast；
- 是否仍有重复 scroll owner；
- overlay close 顺序；
- async race；
- refresh/theme/images 是否重建 foreground；
- destroyed renderable、listener、timer 泄漏；
- native OpenTUI API 是否正确使用；
- 测试是否验证真实用户行为；
- 性能和 render churn；
- public contract 是否意外泄漏 internal native 类型。

review 输出必须 findings first，按 P0/P1/P2 排序，并带文件和行号。主 agent 必须逐项修复或明确解释不修原因，再进行第二轮 review。

## 7. 分阶段实施

### Phase 0：基线和 native spike

目标：在不改变产品主路径前确认 OpenTUI native API 足够覆盖需求。

任务：

1. 记录行数、依赖图、focus owner、key listener、scroll owner；
2. 使用 native `CliRenderer` 创建最小 shell；
3. 验证 Box、Textarea、Input、Select、ScrollBox、Markdown、Diff、Image；
4. 验证 native focus events；
5. 验证 `onKeyDown`、`handleKeyPress` 和 global key handler 顺序；
6. 验证 overlay attach 后 focus；
7. 验证 TestRenderer、mock keys、mock mouse；
8. 验证 Bun package 和 standalone binary 的 native asset 加载。

决策：第一版优先使用 imperative native renderables，不强制全面采用 VNode/composition API。流式 transcript 需要高频原地更新，imperative renderable 更直接。VNode 可以用于静态 chrome，但不能为了追求形式上的“原生”引入新的状态同步层。

退出条件：

- spike tests 通过；
- 所需 native API 均已从 node_modules 类型和 runtime 验证；
- 没有猜测 OpenTUI API；
- overlay/focus/key event 顺序已经形成书面 contract。

### Phase 1：native kernel

任务：

1. 建立 renderer bootstrap；
2. 建立 overlay manager；
3. 建立明确的 native slot/factory contract；
4. 改用 OpenTUI TestRenderer；
5. 删除 Bone node/layout/event facade；
6. 保持 coding-agent 暂时可编译，允许短期机械迁移 helper，但不允许建立长期兼容层。

退出条件：

```bash
rg "getNativeNode|BoneNodeFactory" packages/tui/src
```

结果为空。

### Phase 2：核心界面迁移

迁移顺序：

1. shell；
2. composer；
3. sidebar；
4. transcript scroll；
5. top bar/status/footer；
6. messages、markdown、diff、image；
7. transcript factory 的 native view contract。

迁移原则：

- 每迁移一个组件，立即删除它对应的 Bone wrapper；
- 不保留长期 dual path；
- 产品状态不放入 renderer/helper；
- layout 只存在于 native tree；
- 流式更新优先原地 mutation，不 clear/rebuild 整棵 tree。

### Phase 3：extension/dialog 迁移

任务：

1. Extension UI V2 view contract 改为 native factory/instance；
2. 所有 dialog 进入 overlay manager；
3. modal action routing 收敛；
4. MountedSlot 改为 native slot；
5. advanced async lifecycle 收敛；
6. editor replacement 的 focus transfer 明确化；
7. theme/images 改为原地 refresh 或 transcript subtree refresh，不 unbind/bind foreground。

### Phase 4：删除旧体系

完成后执行：

```bash
rg "BoneNode|BoneRenderer|BoneView|BoneRenderContext|BoneTestRenderer|getNativeNode" \
  packages/tui/src packages/coding-agent/src/modes/interactive
```

runtime source 中必须为空。若仍存在，每一处都需要明确说明为何是产品 contract，而不是残留兼容层。

同时删除：

- 无用 facade exports；
- 旧测试 helper；
- 重复 focus controller；
- 重复 dialog close logic；
- 仅用于 facade 的 layout helper；
- broad barrel exports；
- unsafe type assertion。

### Phase 5：性能和体验验收

测量：

- 首屏启动时间；
- 空闲 CPU；
- 60 秒流式输出 CPU/内存；
- 10k transcript item 滚动；
- resize render 次数；
- dialog open/close render 次数；
- 会话切换重建节点数量；
- theme/images refresh 重建节点数量；
- standalone binary startup。

目标不是只追求 benchmark 数字，而是确认架构确实减少 render churn：

- composer 不因 presentation refresh 被销毁；
- extension host 不因 theme/images 被重建；
- transcript 流式更新不做全量 replay；
- scroll 不做双向同步；
- keypress 不广播给所有 pane；
- requestRender 在同一 frame 内合并。

## 8. 必须覆盖的测试

### Focus / dialog

1. composer focus -> `/settings` -> Esc -> 立即输入；
2. composer focus -> searchable selector -> Esc -> 立即输入；
3. composer focus -> input dialog -> Esc -> 立即输入；
4. focused dialog child close 后 focus 返回 composer；
5. 连续开关 10 次，`currentFocusedRenderable` 始终有效；
6. close/abort/timeout/dispose 重复触发只 resolve 一次；
7. async advanced view 取消后完成 factory，不 attach、不夺焦点；
8. editor view replacement 正确转移 native focus；
9. resize 和 theme refresh 中关闭 dialog 不错位。

### Key routing

1. modal Esc 不触发 composer cancel/abort；
2. modal Enter 不提交后台 composer；
3. modal arrows 不改变 sidebar/transcript；
4. modal 普通文字进入 focused input；
5. composer 普通文字只进入当前会话 draft；
6. pane navigation 只改变一个 semantic/native focus state。

### Scroll / layout

1. transcript scrollbar 与 transcript 内容同一 viewport；
2. sidebar scrollbar 与会话列表同一 viewport；
3. mouse wheel 只滚动 pointer 所在 native ScrollBox；
4. resize 后 composer/footer 不位移；
5. sidebar drag resize 正常；
6. sticky follow 只在用户未主动向上滚动时启用；
7. 会话切换恢复各自 scrollTop。

### Session isolation

1. A 输出时切到 B，A 继续在 sidebar preview 更新；
2. 切回 A 能看到完整持续输出；
3. A/B draft、live event、history、tool state 不串流；
4. message_end/persistence 竞态不丢最终消息；
5. durable history watermark 不使用未经 persistence ack 的 event revision 代替。

最后两项来自此前独立 review，虽然不属于 UI facade 删除本身，但会直接影响切换会话后的 transcript 正确性，不能在重构中丢失。

## 9. 验证命令

遵循仓库 `AGENTS.md`：

- 修改测试文件后运行对应测试；
- 不直接运行完整 vitest suite；
- 非 e2e 全量使用 `./test.sh`；
- 代码变更后最终运行 `npm run check`，完整处理 error、warning、info；
- 除非用户要求，不运行 `npm run build` 或 `npm test`；
- 不自动 commit。

阶段性命令示例：

```bash
./node_modules/.bin/tsgo --noEmit
bun test ./packages/tui/test/opentui-renderer.bun.ts
node ../../node_modules/vitest/dist/cli.js --run test/opentui-extension-host.test.ts
node ../../node_modules/vitest/dist/cli.js --run test/opentui-interactive-mode.test.ts
git diff --check
```

最终：

```bash
npm run check
./test.sh
```

真实交互通过 tmux 验收：

```bash
tmux new-session -d -s bone-opentui-refactor -x 120 -y 36
tmux send-keys -t bone-opentui-refactor "./pi-test.sh" Enter
tmux capture-pane -t bone-opentui-refactor -p
```

至少验证：

- slash dialog Esc 后立即输入；
- sidebar keyboard/mouse 切换；
- sidebar resize；
- transcript scroll；
- 后台会话流式预览；
- 切回运行中会话；
- theme/images；
- terminal resize；
- package/standalone binary smoke（发布前）。

## 10. 量化验收标准

重构完成需满足：

| 指标 | 目标 |
| --- | --- |
| `BoneNode*` runtime 类型 | 0 |
| `BoneRenderer` runtime abstraction | 0 |
| `getNativeNode()` | 0 |
| 自定义 Bone TestRenderer | 0 |
| native focus owner | 1 |
| overlay lifecycle manager | 1 |
| transcript scroll owner | 1 |
| sidebar scroll owner | 1 |
| modal close implementation | 1 |
| presentation refresh 全 foreground rebind | 0 |
| 普通 keypress 全 pane 广播 | 0 |
| `packages/tui/src/opentui` 代码量 | 删除或降至约 300-600 行 |
| typecheck | 通过 |
| focused OpenTUI tests | 通过 |
| `npm run check` | 通过 |
| `./test.sh` | 通过 |
| tmux smoke | 通过 |
| independent review P0/P1 | 0 |

代码量目标是架构约束，不是机械 KPI。若超过目标，必须说明保留的代码属于 Bone 产品能力，而非 OpenTUI 平行抽象。

## 11. 禁止事项

- 不继续完善 `BoneNode` facade；
- 不增加新的 `Bone*Renderable`；
- 不增加第三套 focus controller；
- 不通过 `setTimeout(() => focus())` 修复焦点；
- 不通过 repeated `requestRender()` 掩盖 layout 错位；
- 不在 dialog close 后盲目调用多个 focus API；
- 不在 mount/constructor 中隐式 focus 未 attach tree；
- 不为迁移长期保留两条 runtime path；
- 不通过 unsafe assertion 将 `BoneView` 假装成 native view；
- 不清空并重建整个 foreground 来刷新 theme/images；
- 不修改或回滚其他 agent/用户的无关改动；
- 不使用 `git reset --hard`、`git checkout .`、`git stash`、`git add .`；
- 不在没有独立 review 的情况下宣布完成。

## 12. 主线程启动指令

主线程收到本文件后应：

1. 完整阅读本文件和仓库根 `AGENTS.md`；
2. 只读审计 git status/diff；
3. 建立执行 plan，并将 Phase 0 标记为 in progress；
4. 创建/确认 `codex/opentui-native-refactor` worktree 或分支，但不得破坏未提交工作；
5. 创建 Agent A、B、C，并给出互不覆盖的文件所有权；
6. 主 agent 自己负责 migration matrix 和集成边界；
7. Phase 0 native spike 通过后再开始广泛迁移；
8. 每个 agent 完成后先运行其 focused tests；
9. 主 agent 合并并运行最终验证；
10. 创建独立 Agent D review；
11. 修复 review findings；
12. 再进行一次 review/verification；
13. 向用户报告代码删除量、架构变化、测试、性能和剩余风险；
14. 用户未要求 commit 时不得自动 commit。

主线程不能把本计划理解为“先把当前 focus bug 修好再重构”。当前 focus bug 是新架构的验收用例，应通过删除重复抽象自然解决。
