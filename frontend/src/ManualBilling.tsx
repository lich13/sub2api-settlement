import { useEffect, useRef, useState } from "react";
import { Select } from "./Select";
import { billedMoney, consumption, date } from "./api";
import type { BillPreview, ManualBasis } from "./types";

export const ratePattern = /^(?:0|[1-9][0-9]*)(?:\.[0-9]{1,6})?$/;
export function useManualBilling(initial?: ManualBasis) {
  const [source,setSource]=useState<ManualBasis["source"]>(initial?.source||"upstream");
  const [enabled,setEnabled]=useState(!!initial);
  const [rates,setRates]=useState<Record<number,string>>(()=>Object.fromEntries(initial?.rates.map(r=>[r.account_id,r.rate])||[]));
  const [selected,setSelected]=useState<number[]>(initial?.ambiguous_usage_ids||[]);
  const [diagnostics,setDiagnostics]=useState<BillPreview|null>(null);
  const touched=useRef(false);
  useEffect(()=>{if(initial&&!touched.current){setSource(initial.source);setEnabled(true);setRates(Object.fromEntries(initial.rates.map(r=>[r.account_id,r.rate])));setSelected(initial.ambiguous_usage_ids);}},[initial]);
  const basis:ManualBasis|undefined=enabled||source==="local_only"?{source,rates:Object.entries(rates).filter(([,rate])=>rate.trim()!=="").map(([account_id,rate])=>({account_id:Number(account_id),rate:rate.trim()})),ambiguous_usage_ids:selected}:undefined;
  const valid=!basis||basis.rates.every(r=>ratePattern.test(r.rate));
  return {source,enabled,rates,selected,diagnostics,setDiagnostics,basis,valid,
    sourceChange(next:ManualBasis["source"]){touched.current=true;setSource(next);setSelected([]);setDiagnostics(null);if(next==="local_only")setEnabled(true);},
    enabledChange(next:boolean){touched.current=true;setEnabled(next);setSelected([]);setDiagnostics(null);},
    rateChange(id:number,rate:string){touched.current=true;setRates(prev=>({...prev,[id]:rate}));setDiagnostics(prev=>prev?{...prev,manual_summary:undefined}:null);},
    select(id:number,checked:boolean){touched.current=true;setSelected(prev=>checked?[...new Set([...prev,id])]:prev.filter(x=>x!==id));setDiagnostics(prev=>prev?{...prev,manual_summary:undefined}:null);},
    clearDiagnostics(resetSelection=true){if(resetSelection)setSelected([]);setDiagnostics(null);},
  };
}
export function ManualBillingEditor({manual,accounts,changed,disabled=false,proportional=true}:{manual:ReturnType<typeof useManualBilling>;accounts:{account_id:number;name:string}[];changed:()=>void;disabled?:boolean;proportional?:boolean}) {
  const [limit,setLimit]=useState(50);
  const ambiguous=manual.diagnostics?.local_unmatched?.filter(r=>r.reason==="ambiguous")||[];
  return <section className="manual-billing">
    <label>账单来源<Select disabled={disabled} value={manual.source} onChange={e=>{manual.sourceChange(e.target.value as ManualBasis["source"]);changed();}}><option value="upstream">使用上游账单</option><option value="local_only">仅用本地记录</option></Select></label>
    {manual.source==="upstream"&&<label className="checkline"><input type="checkbox" disabled={disabled} checked={manual.enabled} onChange={e=>{manual.enabledChange(e.target.checked);changed();}}/>缺失请求补计</label>}
    {(manual.enabled||manual.source==="local_only")&&<>
      <div className="manual-rates">{accounts.map(a=>{const summary=manual.diagnostics?.manual_summary?.accounts.find(s=>s.account_id===a.account_id);return <div className="manual-rate" key={a.account_id}><label><span>{a.name}</span><input aria-label={`${a.name} 补计倍率`} disabled={disabled} inputMode="decimal" placeholder="不补计" value={manual.rates[a.account_id]||""} onChange={e=>{manual.rateChange(a.account_id,e.target.value);changed();}}/></label>{summary&&<p className="field-hint">{summary.request_count} 条 · 原始消费 {consumption(summary.local_amount)} · 补计 {billedMoney(summary.amount)} · 排除 {summary.excluded_count} 条</p>}</div>;})}</div>
      <p className="field-hint">本地原始消费 × 倍率；留空不补计，0 表示不计费。最多六位小数。</p>
      {!manual.valid&&<p className="form-error" role="alert">倍率须为非负数，最多六位小数</p>}
      {!!ambiguous.length&&<details className="manual-ambiguous"><summary>歧义请求 · {ambiguous.length} 条</summary><p className="field-hint">核对后逐条选择；明确匹配的请求不会重复补计。</p>{proportional&&<p className="field-hint">歧义费用可能已包含在上游总额中，勾选前请核对，系统不能自动去重。</p>}{ambiguous.slice(0,limit).map(r=><label className="checkline manual-request" key={r.usage_id}><input aria-label={`补计请求 ${r.usage_id}`} type="checkbox" disabled={disabled||!manual.rates[r.account_id]?.trim()} checked={manual.selected.includes(r.usage_id)} onChange={e=>{manual.select(r.usage_id,e.target.checked);changed();}}/><span>{accounts.find(a=>a.account_id===r.account_id)?.name}<small>{date(r.created_at)} · {r.model||"模型未记录"}</small><small>原始消费 {consumption(r.local_amount)} · 匹配歧义</small></span></label>)}{limit<ambiguous.length&&<button type="button" className="text-button" onClick={()=>setLimit(n=>n+50)}>显示更多</button>}</details>}
    </>}
  </section>;
}
