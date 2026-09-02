import type { AnyForgeEvalReport, LiveForgeEvalReport } from "./types.ts";

function escapeHtml(value: unknown): string {
	return String(value)
		.replaceAll("&", "&amp;")
		.replaceAll("<", "&lt;")
		.replaceAll(">", "&gt;")
		.replaceAll('"', "&quot;")
		.replaceAll("'", "&#39;");
}

function resultClass(passed: boolean): string {
	return passed ? "pass" : "fail";
}

function renderLiveReport(report: LiveForgeEvalReport): string {
	const rows = report.cases.map((result) => `<tr><td><code>${escapeHtml(result.id)}</code></td><td>${escapeHtml(result.run)}</td><td><span class="${resultClass(result.passed)}">${result.passed ? "PASS" : "FAIL"}</span></td><td>${result.metrics.toolCallCount}</td><td><details><summary>断言与轨迹</summary><ul>${result.assertions.map((assertion) => `<li class="${resultClass(assertion.passed)}">${assertion.passed ? "PASS" : "FAIL"} ${escapeHtml(assertion.name)}${assertion.detail ? ` <small>${escapeHtml(assertion.detail)}</small>` : ""}</li>`).join("")}</ul><pre>${escapeHtml(JSON.stringify(result.trace, null, 2))}</pre></details></td></tr>`).join("");
	const metric = (value: number | null, percent = false) => value === null ? "n/a" : percent ? `${(value * 100).toFixed(1)}%` : value.toFixed(1);
	return `<!doctype html><html lang="zh-CN"><head><meta charset="utf-8"><meta name="viewport" content="width=device-width, initial-scale=1"><title>Forge live model evaluation</title><style>body{margin:0;background:#f7f9fa;color:#18212b;font:15px/1.55 system-ui,-apple-system,"Segoe UI",sans-serif}main{width:min(1200px,calc(100% - 32px));margin:40px auto 80px}h1{color:#153b52;line-height:1.2}.summary{display:flex;flex-wrap:wrap;gap:12px;margin:24px 0}.metric{background:#fff;border:1px solid #d8e0e6;padding:14px 18px}.metric strong{display:block;color:#153b52;font-size:1.35rem}.metric span{color:#5c6875;font-size:.85rem}table{width:100%;border-collapse:collapse;background:#fff;border:1px solid #d8e0e6}th,td{padding:12px 14px;border-bottom:1px solid #d8e0e6;text-align:left;vertical-align:top}th{background:#edf3f5;color:#153b52}tr:last-child td{border-bottom:0}.pass{color:#087f77}.fail{color:#a33d36}code,pre{font-family:ui-monospace,SFMono-Regular,Menlo,monospace}pre{max-width:760px;max-height:360px;overflow:auto;padding:12px;background:#14242c;color:#e6f0ef;white-space:pre-wrap;word-break:break-word}small{color:#5c6875}ul{padding-left:20px;margin:8px 0}@media(max-width:700px){table{font-size:.86rem}th,td{padding:9px}.summary{display:grid;grid-template-columns:1fr 1fr}}</style></head><body><main><h1>Forge live model evaluation</h1><p>真实模型 + 真实 Agent loop + Fake Forge Service。不会访问或修改 GitHub/GitLab。指标是当前契约在该模型、提示和重复次数下的硬事件结果，不是模型泛化能力。</p><p>模型：<code>${escapeHtml(`${report.model.provider}/${report.model.id}`)}</code> · runs=${escapeHtml(report.configuration.repetitions)} · max turns=${escapeHtml(report.configuration.maxTurns)} · timeout=${escapeHtml(report.configuration.timeoutMs)}ms</p><div class="summary"><div class="metric"><strong>${report.summary.passed}/${report.summary.total}</strong><span>任务完成</span></div><div class="metric"><strong>${metric(report.summary.taskCompletionRate, true)}</strong><span>任务完成率</span></div><div class="metric"><strong>${metric(report.summary.firstToolSelectionRate, true)}</strong><span>首个工具选择</span></div><div class="metric"><strong>${metric(report.summary.firstCallValidRate, true)}</strong><span>首调参数合法</span></div><div class="metric"><strong>${metric(report.summary.correctionSuccessRate, true)}</strong><span>纠错成功率</span></div><div class="metric"><strong>${metric(report.summary.deterministicRepeatRate, true)}</strong><span>确定性重复率</span></div><div class="metric"><strong>${metric(report.summary.meanToolCalls)}</strong><span>平均工具调用</span></div><div class="metric"><strong>${Math.round(report.summary.meanContextBytes).toLocaleString()}</strong><span>平均上下文字节</span></div></div><table><thead><tr><th>案例</th><th>run</th><th>结果</th><th>工具调用</th><th>详情</th></tr></thead><tbody>${rows}</tbody></table></main></body></html>`;
}

export function renderForgeEvalReport(report: AnyForgeEvalReport): string {
	if (report.mode === "live") return renderLiveReport(report);
	const rows = report.cases
		.map(
			(result) => `
      <tr>
        <td><code>${escapeHtml(result.id)}</code></td>
        <td>${escapeHtml(result.category)}</td>
        <td><span class="${resultClass(result.passed)}">${result.passed ? "PASS" : "FAIL"}</span></td>
        <td>${result.assertions.filter((assertion) => assertion.passed).length}/${result.assertions.length}</td>
		<td><details><summary>查看断言</summary><ul>${result.assertions
				.map(
					(assertion) =>
						`<li class="${resultClass(assertion.passed)}"><strong>${assertion.passed ? "✓" : "×"}</strong> ${escapeHtml(assertion.name)}${assertion.detail ? ` <small>${escapeHtml(assertion.detail)}</small>` : ""}</li>`,
				)
				.join("")}</ul></details><details><summary>查看完整轨迹</summary><pre>${escapeHtml(JSON.stringify(result.trace, null, 2))}</pre></details></td>
      </tr>`,
			)
		.join("");
	return `<!doctype html>
<html lang="zh-CN"><head><meta charset="utf-8"><meta name="viewport" content="width=device-width, initial-scale=1">
<title>Forge scripted eval report</title>
<style>
body{margin:0;background:#f7f9fa;color:#18212b;font:15px/1.55 system-ui,-apple-system,"Segoe UI",sans-serif}
main{width:min(1100px,calc(100% - 32px));margin:40px auto 80px}h1{color:#153b52;line-height:1.2}
.summary{display:flex;flex-wrap:wrap;gap:12px;margin:24px 0}.metric{background:#fff;border:1px solid #d8e0e6;padding:14px 18px}.metric strong{display:block;color:#153b52;font-size:1.6rem}.metric span{color:#5c6875;font-size:.85rem}
table{width:100%;border-collapse:collapse;background:#fff;border:1px solid #d8e0e6}th,td{padding:12px 14px;border-bottom:1px solid #d8e0e6;text-align:left;vertical-align:top}th{background:#edf3f5;color:#153b52}tr:last-child td{border-bottom:0}.pass{color:#087f77}.fail{color:#a33d36}code,pre{font-family:ui-monospace,SFMono-Regular,Menlo,monospace}pre{max-width:620px;max-height:360px;overflow:auto;padding:12px;background:#14242c;color:#e6f0ef;white-space:pre-wrap;word-break:break-word}small{color:#5c6875}ul{padding-left:20px;margin:8px 0}details+details{margin-top:8px}@media(max-width:700px){table{font-size:.86rem}th,td{padding:9px}.summary{display:grid;grid-template-columns:1fr 1fr}}
</style></head><body><main>
<h1>Forge scripted evaluation</h1><p>这是协议/工具链合规评估，不代表通用模型智能，也不代表真实 GitHub/GitLab 版本兼容性。</p>
<div class="summary"><div class="metric"><strong>${escapeHtml(report.summary.passed)}/${escapeHtml(report.summary.total)}</strong><span>通过案例</span></div><div class="metric"><strong>${escapeHtml((report.summary.protocolPassRate * 100).toFixed(1))}%</strong><span>协议通过率</span></div><div class="metric"><strong>${escapeHtml(report.summary.failed)}</strong><span>失败案例</span></div></div>
<p>生成时间：<code>${escapeHtml(report.generatedAt)}</code> · schema v${escapeHtml(report.schemaVersion)} · mode ${escapeHtml(report.mode)}</p>
<table><thead><tr><th>案例</th><th>类别</th><th>结果</th><th>断言</th><th>详情</th></tr></thead><tbody>${rows}</tbody></table>
</main></body></html>`;
}
