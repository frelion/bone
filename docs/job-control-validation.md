# Job 控制重构验收（2026-09-15）

## 范围和判定

保留 Router / Worker / Kernel / Runtime 分层，交付有界提案纠错、原子 Delegate + WaitAll、条件唤醒、叶子默认权限及祖先共享子树预算。模型接口实验未通过真实任务，已经撤回。协议和限制见 [Job 控制协议](job-control-protocol.md)。

使用本机 Podman 的既有 `dogpaddle-sink-test` 连接、Ubuntu 24.04 容器及已登录的 ChatGPT subscription；模型为 `gpt-5.6-luna`，每题上限 120 秒。没有切换全局 Podman 默认连接，没有停止现有数据库服务，没有运行 API-key 计费测试。

“完整通过”必须同时满足独立产物判题、正常 Completed 终态、要求的 Job 结构。不能将“文件正确但超时”记为成功，也不能把失败提前退出当加速。自主模式的结构检查只确认单根；它不能证明拆分质量最优。

每个条件仅一个样本，服务延迟及模型随机性明显。部分对照与 Linux 构建共享宿主资源，宿主还有无关服务；这些耗时不是严格性能基准，不能据此宣称稳定加速比例。确定性测试用于证明状态机保证，真实小题用于检查端到端行为，两者不混用。

## 旧版对照

二进制 SHA256：`fa4eee5524df1b7a3a11a54b1610b904994970d9f762a29b1192b201ab1a7a8f`。
原始记录：[baseline](../benchmarks/results/job-control/baseline/20260915T004501Z/summary.json)。

| 条件 | 产物 | 终态 | 结构 | 子 Job | 父 Worker 轮数 | 运行秒数 |
| --- | --- | --- | --- | ---: | ---: | ---: |
| single-single | 通过 | completed | 通过 | 0 | 5 | 32.945 |
| independent-delegated | 通过 | timeout | 不通过 | 4 | 13 | 120.042 |
| independent-auto | 不通过 | failed | 通过 | 1 | 6 | 37.887 |
| dependent-delegated | 通过 | timeout | 不通过 | 5 | 11 | 120.039 |

完整通过 1/4。独立委派题实际重复创建两份同名 normalize/dedupe 子任务；自主题再次触发 `delegate requires at least one assignment`，此前的整 Job 失败策略直接终止任务。这些是观测到的故障，而不是仅凭提示词推测。

## 被撤回的独立动作函数实验

SHA256：`9d57249a8eb856ea750c111113f4b9aa7bc9b446798fdc098a0e2fc36f75145f`。
原始记录：[native functions](../benchmarks/results/job-control/candidate-native/20260915T005126Z/summary.json)。

将 Worker 的嵌套联合类型拆成多个原生函数，要求恰好一个函数且关闭 OpenAI parallel tool calls。离线全仓及请求组装测试通过，但真实四题完整通过 **0/4**：三个条件出现 `model must return exactly one available work action call`；独立委派题产物通过，但一个子任务没有满足原定交付，父任务正确报告 Failed。

现有日志没有保留 provider 原始响应，因此不能断言这些错误是零调用、多调用还是错误函数名，也不能断言是 SDK/schema bug。该方案不合入；没有通过放宽响应基数或执行多个动作来掩盖失败。

## 保留方案与最终验收

最终源码重建的 Linux 二进制 SHA256 为 `32dab9390882ed7074db24c80a96220fbd90a52ebb7b89161f683b85c4263e44`，与撤回接口实验前保留的二进制逐字节相同。
原始记录：[前两题 smoke](../benchmarks/results/job-control/candidate-smoke/20260915T002957Z/summary.json)、[后两题](../benchmarks/results/job-control/candidate-remaining/20260915T005537Z/summary.json)。它们使用相同二进制和未加内核上限的原始 prompt-only 条件。

| 条件 | 产物 | 终态 | 结构 | 子 Job | 父 Worker 轮数 | 运行秒数 |
| --- | --- | --- | --- | ---: | ---: | ---: |
| single-single | 通过 | completed | 不通过 | 1 | 13 | 71.935 |
| independent-delegated | 通过 | completed | 通过 | 2 | 3 | 63.829 |
| independent-auto | 不通过 | timeout | 通过 | 4 | 8 | 120.068 |
| dependent-delegated | 通过 | completed | 通过 | 2 | 4 | 117.716 |

完整通过 **2/4**，不是整体策略效率已解决的证明。独立委派题有 27.559 秒子模型调用重叠，父 Worker 仅三轮；依赖题两次 `Delegated` 都是 `WaitAll`，父 Worker 四轮，没有子模型重叠。它们证明本次样本中原子委派/等待路径被实际使用；与旧版轮数相比明显减少，但不构成统计性能结论。

单 Job 的结构失败保留：模型创建了一个 goal 为 “Do not execute” 的占位子任务，其 done_when 自称与用户禁止委派的要求冲突。自主题连续创建三次相近的只读检查，最后才派实现任务并超时；记录没有 `WorkRejected`。这是合法但重复的规划，**不同于非法动作**，纠错预算不会触发。预算限制扩张，不能判断语义重复。

最后单独使用 `--enforce-job-contract` 验证宿主限制；它不替代上述 prompt-only 对照，也不强制模型一定成功。[最终显式约束验收](../benchmarks/results/job-control/final-contract/20260915T010012Z/summary.json) **3/3 完整通过**，二进制 SHA256 与上文相同。

| 条件 | 数量/深度上限 | 产物/终态/结构 | 子 Job | 父 Worker 轮数 | 运行秒数 |
| --- | --- | --- | ---: | ---: | ---: |
| single-single | 0 / 1 | 全部通过 | 0 | 5 | 36.945 |
| independent-delegated | 2 / 2 | 全部通过 | 2 | 4 | 104.674 |
| dependent-delegated | 2 / 2 | 全部通过 | 2 | 6 | 107.695 |

并行题子模型调用重叠 36.878 秒；依赖题无重叠，第二个子 Job 在第一个终态后创建。所有 trial 容器已在留证后清理，既有数据库服务未改动。

其中显式约束的 independent-delegated 出现了真实的“非法引用 → 同 Job 纠正 → 完成交付”：Job 3 的 Call 8、Call 9 分别在 record 32、40 被拒绝，原因都是 `worker cannot read that record`，连续计数达到 2；Call 12 改为合法 `read /app/dedupe.py`，随后 apply_patch、bash 验证，Job 3 在 record 70 Completed。没有放宽读权限，没有新建替代 Job，也没有因前两次拒绝直接让整个任务失败。父 Job 的单次 `Delegated` 同时创建 `[2,3]` 并 `WaitAll`。

## 本地回归与复现

最终源码通过 `cargo test --workspace --quiet`、`cargo clippy --workspace --all-targets -- -D warnings`、`cargo fmt --all -- --check` 和 `git diff --check`。其中 Core 123、adapters 单元 114、app 单元 141、TUI 单元 325 通过，另有 workspace 集成测试通过。Python runner 使用 Python 3.14 执行 `python3 -m unittest discover -s benchmarks/tests -q`，16 项通过。API-key live certification 保持 ignored。

新增确定性用例覆盖拒绝无副作用、连续/累计预算、取消与重复事件、输入审阅、原子等待、部分成功不唤醒、失败/询问打断、共享祖先额度、不返还额度、批量权限原子拒绝、宿主收紧、旧快照迁移。Python 用例验证 opt-in 上限不会改变 fixture/prompt、零预算透传、诊断只读且不导出其他 namespace、诊断失败仍保留结果并清理容器。

复现最终的显式约束验收：

```bash
CONTAINER_CONNECTION=dogpaddle-sink-test python3 -m benchmarks.architecture \
  --binary target/behavior-linux/job-control-final/release/bone \
  --attempts 1 --timeout-seconds 120 --enforce-job-contract \
  --case single-single --case independent-delegated --case dependent-delegated \
  --results-dir benchmarks/results/job-control/final-contract
```

此命令要求 Python 3.11+。不传 `--enforce-job-contract` 才是 prompt-only 对照；不能将两种约束混算成功率或性能。

## 可保证和不可保证的边界

- 提案校验拒绝不提交 note/report/answers/action 或输入审阅确认；同 Job 可纠错，连续三次/累计八次终止。重复、取消、失效结果不消耗新的纠错次数。
- Delegate + WaitAll 原子提交；部分成功不额外唤醒父模型，但记录仍需最终审阅。失败/取消、询问、新输入按规则打断；结果投递去重。
- 叶子默认无委派权限；后代创建消耗所有祖先的共享额度，完成不退款。宿主 0/1 禁止子 Job，2/2 至多两个直属子 Job，无孙 Job。
- 数量上限不强制恰好创建两份，也不验证自然语言顺序、文件 scope 或语义重复。默认允许 Job 工作，不保证每次自主拆分都合理。
- 恢复中断未完成 Job，不重放未知外部写；未实现可恢复工作流。provider/解码失败仍按原策略处理，不在本次纠错机制内。
- 原始结果目录为本机 gitignored 工件；包含轨迹、判题、二进制指纹及（除早期 smoke 外）只读导出的内核记录，不包含认证缓存。分享前仍需检查工具参数和模型笔记中的任务数据。
