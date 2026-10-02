# BONE TUI 盲发散：独立初稿与对抗修订

日期：2026-10-02。唯一 owned 文件：本文件。独立稿未读取现有 TUI 截图、requirements、旧 reports 或代码。已知技术边界仅 Rust 单 package、Ratatui、80×24 与 120×40。本文提出操作模型，尚未声称实现可行或通过用户验证。

## 0. 防锚定程序与统一情景

先推导三个不依赖随机词的基础交互模型，再接受主任务预注册的 seed `a84881627a3cced0339b77f8` 与词表映射 **潮汐／铅字排版／舞台后台**。没有重新抽词，没有把随机词换成更好解释的词。每词强迫三个改变行为的机制；配色与比喻标题不计机制。之后选四个两两操作模型不同的候选。

独立推导顺序是基础模型→九个机制→四个方向与失败条件→原始文献校验。研究用来检验推断，未照抄非 AI 应用整体布局。独立稿写入后才读其他组报告。

统一情景：修 Auth 过期→运行20秒只读验证→第8秒用户说“只解释先别写”→验证的编译阶段失败→用户查看错行→Agent问“过期时刷新一次还是退出？”→用户回答“刷新一次”→用户另行说“继续改并验证”→修复、验证、交付。**回答业务问题不解除“先别写”。** 构建失败来自此前已开始的只读验证，不意味着 steer 后启动了新的写操作。

八份截图均截在“构建失败、正在读错行、问题未答”的同一时刻。每份80×24或120×40均按字符格生成；英文是测量语料，产品可本地化。中文指令原句保留。`>`是选择/焦点，`_`是输入光标，`[...]`是可读详情的摘要，`?`是未知。代码块行尾空格属于测量网格。

## 1. 无随机的三种基础交互模型

| 模型 | 用户主动作 | 稳定单位 | 优点 | 固有代价 |
|---|---|---|---|---|
| B0-1 连续工作账本 | 读顺序记录、指向一段、回复 | 事件锚点 | 因果自然 | 数小时工具输出冲走重点 |
| B0-2 固定工作面 | 在意图、变化、证据之间切换关注 | 当前工作区块 | 当前状态易找 | 焦点与窄屏降级复杂 |
| B0-3 结果集合 | 选问题、错误或改动，然后行动 | 用户有关的对象 | 审阅直接 | “为什么如此”的叙述分散 |

始终只有一个 Agent。内部多 Jobs 可以同时产生证据，但不形成用户可分配或启动的 Job 目录、成员列表、标签页、并行控制台。

## 2. 随机词强迫产生的九项机制

| 词 | 机制 | 用户可观察的行为变化 |
|---|---|---|
| 潮汐 T1 | 输出节律与阅读冻结 | live低频刷新；读历史后正文不流动，只累计新增计数 |
| 潮汐 T2 | 意图提交产生执行边界 | “先别写”提交立即阻止未启动副作用；已开始动作单列收据 |
| 潮汐 T3 | 返场差量 | 回live先看“离开后1失败、0写入、1问题”，可直达变化 |
| 铅字 P1 | 稳定引用 | 折叠/重排后#14仍指同一事件，Q1仍指同一问题 |
| 铅字 P2 | 校样覆盖层 | 看错行/diff覆盖阅读区域；关闭后回原句与折叠状态 |
| 铅字 P3 | 栏位按宽度分工 | 窄屏一份正文；宽屏出处作旁注，旁注不抢正文焦点 |
| 舞台后台 S1 | 下一个副作用提前可见 | “下一步编辑auth.rs，被当前指令阻止”；不展示Job编排 |
| 舞台后台 S2 | 说话作用分离 | ask解释、reply填Q1、steer更改工作方向，提交前显示作用 |
| 舞台后台 S3 | 场景恢复凭据 | 恢复意图版本、上次读点、文件变化、未答问题 |

九项不全部塞入每个方案。T2是正确性底线，其余可以被否决。波浪动画、纸张边框、把运行改叫“涨潮”、把问答改叫“对讲”均不算新机制。

## 3. 共用语义底线

主层级回答：我授权的工作是什么；我的问题有没有答案；文件改变了什么；现有证据能证明什么。工具细节其次。用户不需要从30条日志推算答案。

**保存／纳入／已开始写入必须区分。** 提交指令后，界面先显示“已保存14:03:08；本地已阻止新写入；Agent待确认”，随后显示“Agent已纳入intent v2”。若已有写命令启动，显示“启动14:03:07，结果未知；正在核对文件”，不能用“paused”“0写入”覆盖它。只能核对完成后写“实际改动2文件；后续写入已止”。本例没有已启动写命令，所以可以在验证核对后说0新写入；另一个分支必须显示未知。

- Input是编辑中的文字，不是队列项。IME未完成不发送；粘贴换行不提交。Enter提交，Ctrl-J换行。Esc退出输入保留草稿。非输入视图按i进入输入；文字键仅在非输入上下文导航。
- Ask产生解释，不修改执行意图，页面显示“工作继续”。若文字明确限制工作，提交前必须展示其真实steer作用，不能把“先别写”丢进纯问答旁路。
- Reply绑定Q1。新问题不会把草稿改绑到Q2。回答Q1不释放write fence。过期问题的回答必须标“Q1已关闭；作为补充”，不偷换接收对象。
- Steer提交本地立即冻结尚未启动的副作用，然后等待Agent理解确认。这两个状态分开显示。不能等长回复生成完才停写。
- 已开始只读验证可以结束；已开始写操作不承诺撤销。未知时长只显示已过时间，不显示伪百分比。
- 阅读/搜索/折叠/看错行均不改授权。选区存在时Ctrl-C只复制；无选区时Ctrl-C请求停止当前外部命令。退出用显式/quit，草稿保留。
- 交付含改动、验证、未解决项。“构建失败”必须说明断言未运行，不能用“测试失败”暗示业务断言执行过。

## 4. A：带锚点的线性工作账本

**核心操作模型。** 在一条连续的因果记录中移动。消息、工具、打断按时间排列，选锚点后展开、查看引用或回复。取B0-1与T1/T2/P1/P2。120列的旁注只解释选中文段，不成为另一条live流。

**主层级。** 当前授权/写状态→正在读的正文→绑定问题与证据→输入作用。已读到底才跟随live；读旧块只增计数。新失败不抢焦点。

80×24：#14错行校样；底部草稿保留。

```text
+------------------------------------------------------------------------------+
|BONE auth-expiry / WRITE STOPPED / 0 new writes / +2 new                      |
|Reading #14 of 18   Alt-L live   / search   ? keys                            |
|-- #08 YOU ------------------------------------------------------             |
|Fix Auth expiry; preserve the refresh-token flow.                             |
|-- #09 AGENT ----------------------------------------------------             |
|Expiry guard rejects before refresh. Verifying first.                         |
|-- #10 VERIFY ---------------------------------------------------             |
|[cargo test auth::expiry / 20s / build failed / 146 lines]                    |
|-- #12 YOU / DIRECTION ------------------------------------------             |
|只解释先别写                                                                  |
|#13 saved 14:03:08 / local fence active / Agent accepted v2                   |
|-- #14 BUILD ERROR / SELECTED -----------------------------------             |
|> src/auth.rs:88:17 E0308 expected bool, found Option<bool>                   |
|  86 | let fresh = session.refresh_allowed();                                 |
|  87 | if session.expired() {                                                 |
|  88 |     return fresh;                                                      |
|  89 | }                                                                      |
|-- #15 Q1 -------------------------------------------------------             |
|Expiry: refresh once, or sign out? [r replies; fence remains]                 |
|----------------------------------------------------------------              |
|[STEER] > _                                                                   |
|Enter send  Ctrl-J newline  Esc read  e expand  r reply                       |
+------------------------------------------------------------------------------+
```

120×40：同一锚点，出处放在旁注；没有固定三栏。

```text
+----------------------------------------------------------------------------------------------------------------------+
|BONE auth-expiry / WRITE STOPPED / 0 new writes / +2 new                                                              |
|Reading #14 of 18   Alt-L live   / search   ? keys                                                                    |
|----------------------------------------------------------------------------------------------------------------      |
|#08 YOU: Fix Auth expiry; preserve the refresh-token flow.                 INTENT v1                                  |
|                                                                                                                      |
|#09 AGENT: Expiry guard rejects before refresh.                            SOURCE #07                                 |
|    Verifying existing behavior before changing it.                       src/auth.rs:82-90                           |
|                                                                                                                      |
|#10 VERIFY                                                               Read-only; before fence                      |
|    [cargo test auth::expiry / 20s / build failed / 146 lines]                                                        |
|    Exit 101. No assertions ran.                                          Enter opens raw output                      |
|                                                                                                                      |
|#12 YOU / DIRECTION                                                       INTENT v2                                   |
|    只解释先别写                                                                                                      |
|                                                                                                                      |
|#13 SAVED 14:03:08 / LOCAL FENCE ACTIVE / AGENT ACCEPTED v2                                                           |
|    No new edits. The started read-only verification may finish.                                                      |
|    Next effect: edit auth.rs; withheld until direction changes.                                                      |
|                                                                                                                      |
|> #14 BUILD ERROR / SOURCE PROOF                                           Evidence #10                               |
|    src/auth.rs:88:17 E0308 expected bool, found Option<bool>                                                         |
|    84 | fn expired_path(session: &Session) -> bool {                                                                 |
|    85 |     // Return whether refresh may be attempted.                                                              |
|    86 |     let fresh = session.refresh_allowed();                                                                   |
|    87 |     if session.expired() {                                                                                   |
|    88 |         return fresh;                                                                                        |
|    89 |     }                                                                                                        |
|    90 |     false                                                                                                    |
|    Esc closes proof and returns to the selected #14 line.                                                            |
|                                                                                                                      |
|#15 Q1: Expiry: refresh once, or sign out?                                                                            |
|    r replies only to Q1. Reply leaves intent v2 in force.                                                            |
|                                                                                                                      |
|----------------------------------------------------------------------------------------------------------------      |
|[STEER] > _                                                                                                           |
|Enter send  Ctrl-J newline  Esc read  e expand  r reply  Alt-L live                                                   |
|                                                                                                                      |
|                                                                                                                      |
+----------------------------------------------------------------------------------------------------------------------+
```

**折叠。** Agent文字保留结论，超过一屏给“继续读12行”。工具默认命令/结果/经过时间/输出行数一行；失败露出有效错误与“断言未运行”。重复栈折为“另5条同根错误”。用户输入不归并成“3次输入”。地址在折叠后不变；raw可读、复制、搜索。

**焦点/键位。** Tab正文与输入；正文j/k走锚点，Enter开引用，e展开，r答选中Q1，/搜，Alt-L回live，备用/live。Esc关校样回原锚点，再Esc回正文。Ctrl-K选择ASK/REPLY/STEER，输入头显示作用。已有字草稿从steer切reply须保留原草稿并单独新建reply草稿，不能转移内容到另一个作用。

| 情景 | 页面变化与动作 |
|---|---|
| 修Auth/20秒验证 | #08请求、#09原因、#10计时；可上滚且不移动阅读点 |
| 插话 | #12先显示已保存/本地阻止，#13显示已纳入；已启动写另有收据 |
| 构建失败/错行 | #10失败，#14定位；Enter源码覆盖，Esc回#14 |
| 回答/继续 | r答Q1“刷新一次”，fence保持；另发steer“继续改并验证”建立v3 |
| 交付 | patch、验证、交付独立锚点；交付引用先前错误与修复证据 |

**空/长/失败/未知/恢复。** 空只一句目标提示与输入。长会话显示事件范围，搜索按稳定地址；完成验证可压一行但留证据。失败留原位置，header记未解决1。未知写显示WRITE UNKNOWN与started-before-fence收据。恢复持久化intent/fence/viewport锚点/折叠/草稿，#14没载入就显示未加载，不改指附近段。

**主动删除。** token速度、思考全文、内部Job ID、每次read-file大标题、重复工作中动画。删除显示噪声，raw证据留存。

**三条致命反例。** ①20000事件后找交付还要逐条滚，/changes若不能直达则失败。②按输出到达时间排序把fence前启动动作画成fence后执行，无start/end收据则误导。③ID稳定但折叠/生成重排让阅读视野跳行，同样失败。

## 5. B：原生终端文档与按需阅读器（激进）

**核心操作模型。** 主体是真实terminal scrollback中append-only文档。语义块完成就打印，终端原有选字、复制、滚动仍在。只保留prompt与一行当前状态；/open E1、/find、/changes临时进入Ratatui独占阅读器，Esc原样回prompt。大多数时间不驻alternate screen。取T1/T2/P1/P2/S3。

与A的区别不是边框减少：完成记录由terminal原生管理，结构化阅读是短暂模式。**打印后的文本不能撤回或真正原位折叠**；摘要只表示可打开更多详情，不能声称已打印的146行被收起来。

80×24：由原生/open E1临时进入阅读器；上半不是chat pane。

```text
$ bone work                                                                     
W1: Fix Auth expiry; preserve refresh-token flow.                               
Agent: expiry guard rejects before refresh. Verifying first.                    
V1: cargo test auth::expiry (read-only), started 14:03:00                       
bone[work]> 只解释先别写                                                        
v2 saved / local fence 14:03:08 / Agent accepted                                
V1: build failed after 20s; E0308. No assertions ran.                           
Q1: Expiry: refresh once, or sign out?                                          
bone[explain]> /open E1                                                         
+-- READER E1 / src/auth.rs:88:17 -----------------------------+                
| Expected bool, found Option<bool>                           |                 
|                                                            |                  
| 86 | let fresh = session.refresh_allowed();                  |                
| 87 | if session.expired() {                                  |                
| 88 |     return fresh;                                      |                 
| 89 | }                                                     |                  
|                                                            |                  
| Current file matches V1. Enter opens the captured receipt.   |                
| WRITE STOPPED / 0 new writes / Q1 waiting                    |                
| /reply Q1 answers only; explain-only rule remains.           |                
| Esc returns to original prompt and retained draft.          |                 
+------------------------------------------------------------+                  
                                                                                
READER focus: source  j/k scroll  Enter receipt  Esc return                     
```

120×40：同一阅读器展示长上下文与V1收据。

```text
$ bone work                                                                                                             
W1: Fix Auth expiry; preserve refresh-token flow.                                                                       
                                                                                                                        
Agent: expiry guard rejects before refresh. Verifying existing behavior first.                                          
V1: cargo test auth::expiry (read-only), started 14:03:00                                                               
                                                                                                                        
bone[work]> 只解释先别写                                                                                                
v2 saved 14:03:08 / local write fence active / Agent accepted                                                           
No new edits. The running read-only verification may finish.                                                            
                                                                                                                        
V1: build failed after 20s; exit 101. No assertions ran.                                                                
E1: src/auth.rs:88:17 E0308 expected bool, found Option<bool>                                                           
Q1: Expiry: refresh once, or sign out?                                                                                  
                                                                                                                        
bone[explain]> /open E1                                                                                                 
+-- READER E1 / source --------------------------------------------------------------+                                  
| src/auth.rs:88:17 E0308 expected bool, found Option<bool>                           |                                 
|                                                                                  |                                    
| 82 | impl Session {                                                              |                                    
| 83 |     // Whether an expired session may attempt refresh.                       |                                   
| 84 |     fn expired_path(&self) -> bool {                                         |                                   
| 85 |         let session = self;                                                 |                                    
| 86 |         let fresh = session.refresh_allowed();                               |                                   
| 87 |         if session.expired() {                                              |                                    
| 88 |             return fresh;                                                  |                                     
| 89 |         }                                                                  |                                     
| 90 |         false                                                              |                                     
| 91 |     }                                                                      |                                     
| 92 | }                                                                          |                                     
|                                                                                  |                                    
| V1: read-only; start 14:03:00; end 14:03:20; 146 lines. Enter receipt.              |                                 
| Current file matches V1. Write fence set 14:03:08. 0 new writes.                    |                                 
| Q1 waiting. /reply Q1 answers only; explain-only rule remains.                     |                                  
| Esc returns to original prompt, retained draft, terminal scrollback.              |                                   
| / search   Enter receipt   ? reader keys                                          |                                   
+----------------------------------------------------------------------------------+                                    
                                                                                                                        
READER focus: source  j/k scroll  Enter receipt  Esc return                                                             
                                                                                                                        
                                                                                                                        
```

**层级/折叠。** 每个打印块先结果句再工具凭据。工具raw暂存，完成只打印摘要；20秒等待仅prompt上方一行计时。失败必须append关键错误与地址，不能只更新滚走的一行。长说明按完成段append一次，不打印后回写句尾。

**焦点/键位。** prompt遵循输入编辑键；Ctrl-R搜历史输入，Ctrl-K选作用；/open E1进阅读器，/find expiry查结构记录，/changes审阅，/live查凭据。阅读器独占焦点，j/k读、Enter开收据、/搜、Esc回原prompt。终端选字归terminal；/export生成可复制文本。prompt是BONE输入，退出才归shell。/reply Q1与/ask有明示作用；自然语言限制仍被尊重，不强迫用户记/pause。

| 情景 | 文档/阅读器变化 |
|---|---|
| 修Auth/验证 | append W1与原因；计时一行，不喷146行 |
| 插话 | append已保存/fence收据，然后append Agent已纳入；prompt改explain |
| 已启动写分支 | append W0 started-before-fence/outcome unknown，之后append核对结果，旧文不改写 |
| 构建失败/错行 | append V1失败/E1/Q1；/open E1开阅读器，Esc回原prompt |
| 回答/继续 | /reply Q1答“刷新一次”，prompt仍explain；另发steer解除限制 |
| 交付 | append P1/V2/D1；/open P1审阅，/export D1 |

**空/长/失败/未知/恢复。** 空一条prompt。长历史依赖terminal可见scrollback，但结构记录独立持久化，/find不能依赖terminal是否已截断。失败append可操作地址。未知写append真实收据，核对完成append结果，不涂改过去。恢复先打印intent/fence/未答问题摘要；原scrollback可能没了，/history重建阅读器，不能宣称完整terminal记录已恢复。

**主动删除。** 常驻侧栏、边框、成员表、typing动画、token滴答；阅读器才画边界。

**三条致命反例。** ①异步状态刷新打坏prompt草稿，输出锁/重绘未证实则不能用。②terminal上滚不可感知，关键失败在底部看不见，当前凭据再清楚也可能不达用户。③tmux/SSH/terminal切alternate screen丢scrollback或返回位置，原生模型收益变成恢复负担。

## 6. C：可行动对象工作台

**核心操作模型。** 主页是一组用户有关的对象，选问题/证据/改动/交付/当前意图后行动，没有必须滚到底的chat。对象不是Job。默认先未答决定，但新事件不改变用户当前选择。取B0-3与P2/P3/T3/S1/S2。时间只作对象provenance视图。

80列是目录→全文→返回的深度导航，120列是目录与全文并排；不会把三个pane缩成20字。截图已选E1，所以窄屏目录退成breadcrumb。

```text
+------------------------------------------------------------------------------+
|BONE auth-expiry / explain only / WRITE STOPPED                               |
|Objects > Evidence V1 > Error E1 / 1 question waiting                         |
|----------------------------------------------------------------------        |
|> E1: src/auth.rs:88:17                                                       |
|  E0308 expected bool, found Option<bool>                                     |
|                                                                              |
|  86 | let fresh = session.refresh_allowed();                                 |
|  87 | if session.expired() {                                                 |
|  88 |     return fresh;                                                      |
|  89 | }                                                                      |
|                                                                              |
|  V1: build failed after 20s. No assertions ran.                              |
|  146 raw lines. Enter opens captured V1 receipt.                             |
|  Current file matches V1.                                                    |
|                                                                              |
|  Direction v2: 只解释先别写                                                  |
|  Saved 14:03:08 / local fence / Agent accepted                               |
|  Next effect: edit auth.rs / WITHHELD by v2                                  |
|  Q1: Expiry: refresh once, or sign out? [r answer]                           |
|----------------------------------------------------------------------        |
|Esc objects  Enter evidence  / find  d changes  i speak                       |
|[STEER] > _                                                                   |
+------------------------------------------------------------------------------+
```

```text
+----------------------------------------------------------------------------------------------------------------------+
|BONE auth-expiry / explain only / WRITE STOPPED / 0 new writes                                                        |
|1 question waiting / +1 new evidence / saved 14:03:08 / Agent accepted v2                                             |
|----------------------------------------------------------------------------------------------------------------      |
|OBJECTS                       | E1 / src/auth.rs:88:17                                                                |
|                              | E0308 expected bool, found Option<bool>                                               |
|Decisions                     |                                                                                       |
|  Q1 expiry behavior          | 82 | impl Session {                                                                   |
|                              | 83 |     // Whether refresh may be attempted.                                         |
|Evidence                      | 84 |     fn expired_path(&self) -> bool {                                             |
|  V1 build failed, 20s        | 85 |         let session = self;                                                      |
|> E1 auth.rs:88               | 86 |         let fresh = session.refresh_allowed();                                   |
|                              | 87 |         if session.expired() {                                                   |
|Changes                       | 88 |             return fresh;                                                        |
|  none                        | 89 |         }                                                                        |
|                              | 90 |         false                                                                    |
|Delivery                      | 91 |     }                                                                            |
|  none yet                    | 92 | }                                                                                |
|                              |                                                                                       |
|Current direction             | SOURCE                                                                                |
|  v2 explain only             | Current file matches captured V1.                                                     |
|                              |                                                                                       |
|                              | EVIDENCE V1                                                                           |
|                              | cargo test auth::expiry; exit 101 after 20s.                                          |
|                              | Compile failed. No assertions ran.                                                    |
|                              | Started 14:03:00 before local fence 14:03:08.                                         |
|                              | Read-only; 146 raw lines. Enter opens receipt.                                        |
|                              |                                                                                       |
|                              | CURRENT DIRECTION v2                                                                  |
|                              | 只解释先别写                                                                          |
|                              | Next effect: edit auth.rs / WITHHELD by v2                                            |
|                              |                                                                                       |
|                              | RELATED Q1                                                                            |
|                              | Expiry: refresh once, or sign out?                                                    |
|                              | r answers Q1; write fence remains.                                                    |
|                                                                                                                      |
|----------------------------------------------------------------------------------------------------------------      |
|Tab objects/detail/input  / find  d changes  t causal history  r answer  i speak                                      |
|[STEER] > _                                                                                                           |
+----------------------------------------------------------------------------------------------------------------------+
```

**层级/折叠。** 目录先未答决定、未解决证据、改动/交付；优先级更新标记，不在阅读中强行重排。详情先结果/动作再source。用户方向成intent对象，业务答案成Q1.value，必要说明进入所属对象。raw在V1内开。每对象有来源地址可一跳进因果历史；重复错误合并但保留每次凭据。

**焦点/键位。** 窄屏j/k选目录，Enter进全文，Esc回目录；宽屏Tab目录/详情/输入。/搜对象及来源，d直达改动，t开当前对象因果历史。r只回答明确关联Q1；i默认steer，Ctrl-K改ask/reply。仅选择问题不自动发答案。

| 情景 | 对象变化 |
|---|---|
| 修Auth/验证 | intent v1、原因R1、计时证据V1，用户可一直读R1 |
| 插话/保存 | v2对象立即标saved/local fence，Agent确认才标accepted |
| 已启动写 | W0不确定作用对象占状态，不能放到完成改动中 |
| 失败/错行 | V1 failed派生E1；选E1读source与V1来源，新Q1不抢选 |
| 回答/继续 | Q1.value=刷新一次，v2不变；steer建立v3 |
| 交付 | P1/V2/D1，D1引用P1/V2，E1标resolved保留出处 |

**空/长/失败/未知/恢复。** 空一项“提出目标”，不铺空四栏。长目录只活跃对象与按需归档，搜含历史版本；不把随口提问变ticket。失败对象持续存在。未知workspace作用有独立对象占current状态。恢复选中ID/版本/草稿/fence，对象已解决也继续展示，不将焦点改到别的对象。E1必须区分captured source与current file。

**主动删除。** 首页逐条chat、空工具卡、后台执行器生命周期、完成比例。对象种类最多五类，不按工具再造类别。

**三条致命反例。** ①随口二十问全升级对象，用户陷ticket triage。②错误行漂移却标成“当时错误源码”，必须否决。③“为什么改这块”因果跨四对象，无法一跳读全则审阅失效。

## 7. D：用户可编辑的工作契约（最激进）

**核心操作模型。** 主页是一份用户拥有、Agent受其约束的当前契约。主要动作是改变目标/写规则/业务答案，读证据；消息只是字段变更原始出处。没有chat流，没有对象目录。自然语言形成可见契约变更，必要限制先本地成立；e也可直接改字段。取T2/T3/P2/S1/S2/S3。

默认六段：目标、写规则、业务决定、下一步、证据、结果。前三段用户拥有，后三段Agent只报告。不是让用户学配置语言；Agent不得自行解除限制或把未知答案填成同意。宽屏不新开sidebar chat。

```text
+------------------------------------------------------------------------------+
|BONE work contract v2 / explaining / WRITE STOPPED                            |
|Goal         Fix Auth expiry; preserve refresh-token flow.                    |
|Write rule   NO NEW EDITS / set by you 14:03:08                               |
|Your words   只解释先别写                                                     |
|Acceptance   saved / local fence active / Agent accepted                      |
|----------------------------------------------------------------------        |
|Business Q1  UNANSWERED: refresh once, or sign out?                           |
|Next effect  edit auth.rs / BLOCKED by write rule                             |
|----------------------------------------------------------------------        |
|> Evidence   V1 / BUILD FAILED / 20s / no assertions ran                      |
|             src/auth.rs:88:17 E0308                                          |
|             expected bool, found Option<bool>                                |
|  86 | let fresh = session.refresh_allowed();                                 |
|  87 | if session.expired() {                                                 |
|  88 |     return fresh;                                                      |
|  89 | }                                                                      |
|----------------------------------------------------------------------        |
|Result       No file changes. Q1 waiting.                                     |
|Return mark  v2 / no writes / V1 failed / Q1 waiting                          |
|----------------------------------------------------------------------        |
|e edit field  r answer  Enter proof  / history  i instruction                 |
|[CHANGE DIRECTION] > _                                                        |
+------------------------------------------------------------------------------+
```

```text
BONE work contract v2 / Agent explaining / WRITE STOPPED                                                                
Owner: you / saved 14:03:08 / local fence active / Agent accepted 14:03:09 / 0 new writes                               
----------------------------------------------------------------------------------------------------------------        
GOAL / YOU OWN THIS                                                                                                     
  Fix Auth expiry; preserve the refresh-token flow.                                                                     
  Source W1. Enter history.                                                                                             
                                                                                                                        
WRITE RULE / YOU OWN THIS                                                                                               
  NO NEW EDITS                                                                                                          
  Your words: 只解释先别写                                                                                              
  Started read-only V1 may finish. Any not-yet-started edit is withheld.                                                
  e explicitly changes rule; Q1 reply leaves it unchanged.                                                              
                                                                                                                        
BUSINESS DECISION / YOU OWN THIS                                                                                        
  Q1 UNANSWERED: Expiry: refresh once, or sign out?                                                                     
  r opens reply bound to Q1.                                                                                            
                                                                                                                        
NEXT EFFECT / AGENT REPORTS                                                                                             
  Edit auth.rs after Q1 is answered AND write rule permits.                                                             
  BLOCKED by write rule. No internal Job controls.                                                                      
                                                                                                                        
> EVIDENCE / SELECTED PROOF                                                                                             
  V1 cargo test auth::expiry; build failed after 20s. Exit 101. No assertions ran.                                      
  E1 src/auth.rs:88:17 E0308 expected bool, found Option<bool>                                                          
  84 | fn expired_path(session: &Session) -> bool {                                                                     
  85 |     // Return whether refresh may be attempted.                                                                  
  86 |     let fresh = session.refresh_allowed();                                                                       
  87 |     if session.expired() {                                                                                       
  88 |         return fresh;                                                                                            
  89 |     }                                                                                                            
  90 |     false                                                                                                        
  Read-only V1 started before fence. Current file matches captured evidence.                                            
                                                                                                                        
RESULT / AGENT REPORTS                                                                                                  
  No file changes. Build failure remains. Need Q1 answer and separate direction to edit.                                
RETURN MARK / v2: no writes, V1 failed, Q1 waiting. Enter changes since last visit.                                     
----------------------------------------------------------------------------------------------------------------        
e edit field  r answer  Enter proof  / history  i instruction  Esc return                                               
[CHANGE DIRECTION] > _                                                                                                  
                                                                                                                        
```

**层级/折叠。** 用户授权前三段优先，下一步显示真实作用。证据结论优先，Enter开源码/输出校样，Esc回原字段。ask临时全文不修改契约，必要结论可进入只读解释并附出处。raw变证据收据，消息/history只是字段变化来源。

**焦点/键位。** j/k走字段，e编辑用户字段，r答Q1，Enter看proof，i自然语言指令。编辑write rule底部显示`NO NEW EDITS -> EDITS ALLOWED`，Enter只提交明确变化，不批准未知未来动作。只读字段e提示“Agent报告；i更改方向”。Ctrl-K选ask/reply/direction。/搜版本/原文，Alt-L回当前版本。看旧v1时头显`READING v1 / CURRENT v2`；编辑作用当前v2并给差异，避免误认为旧规则live。

| 情景 | 契约变化 |
|---|---|
| 修Auth/验证 | Goal v1，write rule允许任务内编辑，V1计时；焦点不移动 |
| 插话/保存 | write rule v2本地立即禁止新编辑；saved时间与Agent accepted时间分列 |
| 已开始写 | 当前顶栏WRITE UNKNOWN；证据W0列启动时刻/核对中，禁止新写与过去可能已写分开 |
| 失败/错行 | evidence V1 failed，result说明断言未运行；Enter proof，Esc回Evidence |
| 回答/继续 | Business Q1=刷新一次，write rule仍禁；另说继续才v3允许 |
| 交付 | next effect执行，V2通过，result P1/D1，Enter查证据 |

**空/长/失败/未知/恢复。** 空只Goal字段，需时出现其他段。长只当前契约与未解决项；旧版可追溯，用户限定词保留原文。失败写evidence/result，不改用户Goal冒充成功。未知写保持WRITE UNKNOWN到核对，不能用禁新写推断历史零写。恢复核对intent版本与workspace后显示返场差量，校验不了就“当前文件尚未核对”。

**主动删除。** 消息泡泡、内部plan树、Job编排、反复“收到/正在处理”、未证实比例。只展示下一真实作用及阻止原因，保留用户输入原文。

**三条致命反例。** ①自然语言“先别写”变字段时被缩成“先别写某文件”，结构化制造虚假确定性。②每次闲聊要改契约，学习与思考成本太高。③Agent自行重写用户拥有字段，授权边界崩溃。

## 8. 四者两两操作差异与判定轴

| 配对 | 不可靠换标题消除的区别 |
|---|---|
| A/B | A程序管理锚点块流；B终端原生scrollback为主，结构阅读短暂独占 |
| A/C | A按事件先后进入；C按对象进入，时间作来源 |
| A/D | A发送消息再看顺序影响；D直接变当前契约，消息作依据 |
| B/C | B主要prompt+完成文档；C主要选对象+对象动作 |
| B/D | Bappend原文；D常驻可编辑当前契约，历史不在首页 |
| C/D | C先选处理对象；D先改当前授权/目标/业务答案 |

不按功能数量排序。判定轴：错误继续写入概率；离开一小时恢复成本；看错误/改动的操作数；长读保持位置；80×24语义是否成立。A偏因果，B偏terminal原生性，C偏审阅，D偏授权控制。不能把四者优势合成四栏chat以回避模型选择。

## 9. Primary research：事实与推断分开

### GNU Emacs Compilation（非AI应用）

事实：Compilation mode把可解析错误链接到源码位置，区分移动到错误消息与访问源码，支持从其他buffer调用next-error，并有按选择显示源码的follow模式。[GNU Emacs官方手册](https://www.gnu.org/software/emacs/manual/html_node/emacs/Compilation-Mode.html)

推断：E1/V1地址应直接到错行；选错误与进入源码是两种动作。新错误自动跳转会抢阅读焦点，所以候选只通知，不强制跳。Emacs未证明BONE应采用编辑器整体界面。

### Ableton Live Session/Arrangement（非AI应用）

事实：Arrangement按线性时间组织，Session允许实时选择launch；Session接管后恢复Arrangement需显式Back to Arrangement。Select on Launch可关闭，以便启动时保留当前设备视图。[官方Session手册](https://www.ableton.com/en/manual/session-view/)，[Arrangement手册](https://www.ableton.com/en/manual/arrangement-view/)

推断：后台工作与阅读对象独立，回live是显式动作。对象与线性历史可以同源但入口不同。Session音乐启动控制不该被照搬成用户启动BONE内部Job格子。

### Czerwinski、Horvitz、Wilhite：长任务中断恢复（原作者HCI）

事实：作者的周级diary study记录信息工作者任务交错、中断与恢复，指出软件对复杂长期项目重新进入支持不足。[Microsoft Research原作者论文页](https://www.microsoft.com/en-us/research/publication/a-diary-study-of-task-switching-and-interruptions/)，[论文PDF](https://research.microsoft.com/en-us/um/people/horvitz/taskdiary.pdf)

推断：返场要带上次意图、已读位置、关键增量，用户不应从最新chat倒推。研究对象不是BONE，未把某个数字、最优布局或固定状态栏效果归于该论文。

### Vim quickfix：交叉校验

事实：quickfix保留错误列表，Enter开错误文件；异步追加时文档建议不频繁滚底，避免过多重绘，可留旧列表。[Vim原作者参考手册](https://vimhelp.org/quickfix.txt.html)

推断：错误需要构建批次，不能用最近错误覆盖旧证据；滚底节流仍不能代替真正阅读冻结。仅校验机制，不新增第五模型。

## 10. 后续对抗的验收轨迹

独立稿先落盘，以下列检验不预设胜者。

1. 第8秒steer提交，第8.1秒排队编辑试图启动；另有已开始写命令结果未到。核验saved/local fence/Agent accepted/outcome unknown四个状态，不能只看paused。
2. Q1答“刷新一次”后等10秒不得新改文件，另说“继续改并验证”才解除fence。
3. 10000工具事件、80×24、读第200条、后台构建失败与新问题。原段不移位，返回只展示关键差量。
4. 错误发生后外部改文件，行号漂移。必须区分captured/current，不靠同一行号声称真实性。
5. 任意阅读点用键盘定位patch、看错行、回答、继续再回来，关键动作不能藏在横向截断文字中。
6. 重启时持久化intent比最后打印消息新；恢复依据current版本而非最近stdout，未知文件状态不得显示已安全停止。

## 11. 对抗修订（独立稿写入后追加）

本节在独立初稿写入后追加。读过同组 critique.md、research.md 与 root 的 index.html 源码；未重跑他组fixture，也未改其文件。第4—7节保留独立设计记录，下面明确撤回与收缩处，不能将它们视为已通过方案。

### 11.1 先撤回三个过强承诺

**“cargo test只读”撤回。** 它会写构建产物，有的测试还会改fixture/数据库。工具名称不证明无副作用。独立稿截图里的“read-only V1”应读为本情景**事先限定的验证仍可结束**，不能推广成真实cargo test能力分类。正确新文案是“此前已启动验证；后续源码编辑已阻止；既有作用正在收束”。“0新写入”只有执行证据与文件核对确证时才能显示；核对不到则显示“源码结果未核对”，不能从没收到write工具事件推断零写。本例原稿没有未知写分支的截图，也因此不能用这八份画面证明安全。

**“本地立即fence”是所需执行器行为，非TUI现有事实。** 两组证据指出现有事件只足够证明输入保存/Agent纳入，而不保证物理动作已止。如果引擎没有可验证的执行边界，UI只能显示“已保存；要求停止新编辑；Agent待纳入”，不可宣称LOCAL FENCE ACTIVE。四方向都依赖相同的执行器契约，这不是任何布局胜出的证据。只做文字或CSS不能满足该要求。

**Ctrl-C随选区改变不能当唯一停止入口。** 我的共用底线承认选区Ctrl-C只复制，但不能因此允许用户误认已停。对抗后规定：任何焦点有固定独立停止入口，候选`Ctrl-X s`，出现prefix时明示s停止/Esc取消，命令面板有“停止当前执行”；兼容性未验证。Ctrl-C可保持终端复制行为。停止与“禁止以后编辑”的steer仍有不同作用，不能因为同叫暂停而混同。

### 11.2 对两组方案是否只是现有demo增量的攻击

**critique A：有效修复路线，尚不是新核心模型。** 它保留原有chat+editor+详情，只改摘要预算、状态位置、Esc父子返回、通知生命期。这些修改直接回应D01—D09实测，价值明确；但若叫“完全发散设计”，只是把现有demo中的状态和阅读规则修正到可信。随机词并未让用户改变主动作。第10000事件之后如何直接审阅交付仍依赖命令或不断定位时间线，当前动作常驻不能解决结果发现成本。

**critique B：独占详情从modal改成更大原文层，仍是增量。** 收起editor腾20行对长结果有价值，但第8秒用户正读第132行、已有“解释fresh为什么”的草稿时，失败与Q1到达需要回现场才能发方向；如果回现场后又进核查、再回原证据，层栈与草稿生命周期才是代价。它后来收缩成A可进入读法是诚实收敛，不能继续当第二个完整操作模型参加对比。

**research A：段落分组若由UI猜意图，会创造新错误。** 第8秒先别写，旧V1第20秒失败，UI按输出到达时间把失败放进“只解释”段，暗示这次验证是新方向启动。机械“每消息一段”又把一次合作切三段。“标题来自原句”可确定，**归属**却不因此确定。研究者在第二轮撤回默认按意图收束，是正确修正；修正后A更接近规范的现有chat设计。它值得做，但不是大跨度发散候选的证据。

**research B：120双栏收益不能替80路径作答。** 固定v1文件，同时V1失败、Q1到达；80列需要文件→对话→日志→对话→问题→文件，每次侦察badge都要动作。只有一个固定对象无法同时保有v1文件和失败日志；加tab会增加管理工作。来源/版本真实性是新语义能力，布局本身仍可由现有详情模式扩展。研究者已淘汰B默认方向，赞同此裁决；未来只做显式读法不能再宣称一个独立完整默认方向。

**共同未解。** 两组最有价值的输出是实测状态错误、输入对象生命期和语义返回规则。若根本事件来源仍给“就绪/等待纳入”旧值，新增“现在条”只在更醒目位置说错；任何截图推荐必须先注明这点。

### 11.3 四方向在同一条长日志／草稿／插话／未知写轨迹中逐步受压

统一压力轨迹不是原始“零写”美观情景：

- t0：20秒验证C42开始；本轮此前另一个写命令W0在14:03:07已启动，结果迟迟未到。
- t1：用户在10000行日志的第132原文行阅读，输入对象D7含`解释fresh为什么是Option`，光标在fresh后，选区覆盖Option，目标ASK。只是草稿，对执行无作用。
- t2：用户另开方向输入D8，提交“只解释先别写”。保留D7全文/光标/选区/目标；D8变提交收据I2，显示保存事实。Agent纳入时间稍后到达。
- t3：C42编译失败；W0中断变unknown；Q1到达。阅读位置不移；D7不改绑Q1。当前停止入口与未知写状态仍可到达。
- t4：用户进W0核查；独立表单F1写观察；Esc返回。D7仍是ASK草稿，F1草稿单独保留；核查未完成不恢复。
- t5：r新建R1回复Q1“刷新一次”。答案不释放限制，也不覆盖D7。核查结论完成后仍需显式“继续改并验证”。

输入生命期必须有具体对象，而非一句“草稿保留”：D7是原编辑稿，D8是新方向稿，I2是不可变已提交输入，F1是核查表单稿，R1是问题回复稿。取消各层只回自己的父层，不把这些串到单个take_draft()。这里只定义语义对象，不要求首页显示五个draft标签；回原输入时恢复对应稿。若单一输入组件的数据模型做不到，所有方向都不成立。

| 方向 | 每一步可见对象／阅读返回点／草稿存放 | 具体反例 | 第二轮裁决 |
|---|---|---|---|
| A账本 | t1选C42观察第132原文偏移；D7留输入层。t2 I2新增锚点；当前状态只写已保存/纳入/unknown。t3不跳#14；t4 F1核查覆盖，Esc回C42同偏移+D7；t5R1与Q1绑定 | C42最终输出采用另一event ID，观察行132被最终147行重排，原event+offset锚点指错字。必须有call→观察→最终同一性，差异时显示“原观察不在最终输出” | **保留，但归为增量修复方向**；不声称改变根本模型。与两组A差异不足以算三个独立候选 |
| B原生 | t1terminal选区由宿主管，D7在prompt；t2 D8提交可append收据；t3新失败/unknown可能出现在scrollback底部；t4内部reader有F1可返回prompt，但宿主滚动位置不归BONE | 用户仍选旧stdout，程序新输出时selection/viewport/提示由terminal决定。BONE既无法确定unknown提示可见，也无法不侵入terminal而固定状态面。更强控制又取消原生核心 | **淘汰默认B**。我喜欢它保留terminal本性，但无法证明共同轨迹。内部Ratatui不是外部less，本稿不以“两个stdin进程争抢”淘汰它，而以宿主读位/新异常发现不可保证淘汰 |
| C对象 | t1选V1/captured偏移132，D7留输入；t2 I2 intent对象收据；t3W0不确定作用对象与Q1仅通知，不抢V1；t4 F1核查Esc回V1/D7；t5R1绑定Q1，然后回V1 | 第二次验证通过时若目录按当前文件去重，第一次W0未知项可能消失；“成功最新状态”不能结束未核查旧实例。另：对象按优先级重排会让j/k下一步作用对象突然变化 | **保留为待原型验证的新主模型**。必须按发生实例保持未闭合项，目录不抢选；这只修正确性，不添加文件树/tab。切对象成本是否比账本低，仍未知 |
| D契约 | t1用户正编辑ContractDraft(base v2)，D7是临时ask稿；t2 D8指令提交形成待纳入v3，不覆盖ContractDraft；t3W0 unknown独立于禁止新编辑，Q1占业务行；t4 F1覆盖返回Evidence与D7；t5R1填Q1，v3写规则不变 | 契约草稿基于v2，Agent已接受v3；此时Enter保存旧草稿若覆盖v3，会解除先别写。若显示三版本、冲突差异与独立问题稿，学习成本又很高 | **不接受本轮默认D，保留激进实验**。提交前每字符不生效，旧base提交必须显式处理冲突；未知写不因更改契约而核销。尚未证明比自然输入更容易 |

**不混合。** B淘汰，不把native scrollback强塞A。C若成立就让对象作主页，不能保留一条更大的chat侧栏来逃避解释归属。D若实验就真编辑字段，不能退回只读要求引用却继续叫“用户可编辑契约”。A只做当前已有主模型的可信修复，不因可交付最容易就冒充本轮所有发散的最终胜者。

### 11.4 root比较原型准确度审查

只读审查 [index.html](index.html) 的数据/渲染函数；没有浏览器交互实测，也未修改root原型。

1. **B被误当外部reader。** 我的B由BONE自己的Ratatui短暂独占，始终一个stdin所有者；sourceStream()仍在受管理grid重绘，没有真正terminal scrollback。所以HTML既没呈现B的核心，也不适合用“less争键”直接判它负。但B仍因上述宿主滚动不可控被淘汰，两种理由不能混为一谈。
2. **C被改成固定旧证据双面。** 我的C是“选择问题/证据/改动/交付对象然后行动”，不是“文件树+最近对话+固定旧文件”。objects()与120列左侧最近对话、右侧旧source是research B的并读结构，更接近已经淘汰的双栏阅读方向。若用它证明C需管理pane，结论作用对象错了。C仍有自己的导航代价，必须用目录→全文→返回实测。
3. **D已经移除了决定性动词。** contract()写“字段不能直接授权”，展示原话引用，再由聊天修改要求。这可以是安全的只读要求视图，但不是本稿“e编辑owned字段→预览真实变更→提交版本”的D。先否定核心操作再用静态壳比较视觉，无法证明D值得或不值得做；要么标“D被否决后的只读替代”，要么保留实验动词。
4. **原型总给input标“回答Q1”随step出现。** screen()以step===5直接变reply目标，没有用户选择事件。D7已存草稿时新问题到达，视觉会改目标，正是所有报告反对的隐式改绑。文本框值没丢不等于输入作用没改。
5. **原型的收据不是安全事实。** evidenceLines静态写“采纳只解释后没有新写入”，timeline()称cargo test先前只读，不能承载unknown轨迹。页面已承认不能证明执行器保护，这个边界应同样作用于每一条具体演示文案。
6. **“读旧内容”会用同一旧source覆盖四方向。** 这能演示不滚动，却抹掉A的事件锚点、B的host terminal读位、C对象身份、Dcontract版本；同一个固定reader成功不证明四个模型都成功。

这些是比较效度问题，不要求root把所有实验做成真实应用。至少每张推荐应明确“这块只模拟共用阅读/草稿保全”，不能把同一JavaScript状态演示当四模型的对抗验收。

### 11.5 有意保留的无法确定取舍

**C是否胜过A目前无法确定。** 用户想“为什么过期仍被当有效、查看失败、回复、核对改动”，C可能需要R1解释→E1错误→Q1问题→P1改动四次选择；A顺序读一屏可能一次就懂。反过来，用户离开一小时只想审阅P1/V2，C两次操作就到，A的完整因果账本可能还要找交付。两条路径分别偏向两模型，现有模拟没有真实发现成本数据，不能因为C“新颖”或A“熟悉”裁决。

下一步只建议两个严格区分的可操作原型：A做可信账本；C做对象主页，默认不并排chat。以同一10k日志与W0未知写轨迹验证目标误认、草稿保全、事实解释准确度、错误/patch发现步骤。D另作短实验检验三版本负担；B不再占默认方案原型名额。此建议未授权实现，不修改代码。
