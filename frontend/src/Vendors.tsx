import { ManualBillingEditor, useManualBilling } from "./ManualBilling";
import React, { useEffect, useRef, useState } from "react";
import { X, ChevronDown } from "lucide-react";
import { api, money, costInput, amountPattern, inputDate, fromInput, date, consumption, billedMoney, downloadCsv } from "./api";
import { Select } from "./Select";
import type { Account, Catalog, Cycle, Vendor, VendorPeriod, VendorBillPreview, Overview } from "./types";

export function expandVendorSelection(ids:number[], periods:VendorPeriod[]) {
  const selected=new Set(ids);
  for(const p of periods) { const members=p.members?.map(c=>c.id)||p.member_ids||[];if(members.some(id=>selected.has(id)))for(const id of members)selected.add(id); }
  return [...selected].sort((a,b)=>a-b);
}
export type GroupAction = "create" | "edit" | "add" | "renew";
export function currentPeriod(v: Vendor) { return v.periods.find(p=>p.status==="open" && !p.end) || v.periods.find(p=>p.status==="open"); }
export function groupCandidates(accounts: Account[]) { return accounts.filter(a=>a.kind==="unconfigured" && !a.deleted && !a.vendor_id && a.cycle?.status!=="open"); }

export function VendorGroups({vendors,accounts,admin,query,filter,onSettle,onManage,onDetail,totals,selected=[],onToggle}:{vendors:Vendor[];accounts:Account[];admin:boolean;query:string;filter:string;onSettle:(ids:number[],renew:boolean)=>void;onManage:(id?:number,action?:GroupAction)=>void;onDetail:(id:number)=>void;totals?:Overview["vendor_totals"];selected?:number[];onToggle?:(ids:number[])=>void}) {
  const groups=vendors.filter(v=>!v.archived_at && (filter==="all" || filter==="paid" || (filter==="open" && currentPeriod(v))));
  return <div className="vendor-groups">{groups.map(v=>{
    const p=currentPeriod(v), last=p||v.periods[0];
    const members=(v.current_members || last?.members?.map(c=>({account_id:c.account_id,name:c.current_name||c.name,account_deleted:c.account_deleted,renewal_excluded:false})) || []).filter(m=>!m.account_deleted);
    if(query && !`${v.name} ${members.map(m=>accounts.find(a=>a.id===m.account_id)?.name||m.name).join(" ")}`.toLowerCase().includes(query.toLowerCase())) return null;
    return <details className="vendor-group" key={v.id}>
      <summary>{admin&&onToggle&&p&&<input type="checkbox" aria-label={`选择账号组 ${v.name}`} checked={!!p.members?.length&&p.members.every(c=>selected.includes(c.id))} onClick={e=>e.stopPropagation()} onChange={()=>onToggle(p.members?.map(c=>c.id)||[])}/>}
        <div className="vendor-heading"><strong>{v.name}</strong><small>{members.length} 个账号 · {p?`${date(p.start)} 起 · ${p.start_confirmed?"进行中":"起点待确认"}`:last?"已结算／待续费":"待配置首期"}</small></div>
        <div className="vendor-total"><small>{p?"当期总成本":"上期总成本"}</small><b className="amount">{money(last?.cost_fen??v.cost_fen)}</b></div><span>{last?.creditor_name||v.creditor_name}</span><ChevronDown size={16}/>
      </summary>
      <div className="vendor-members">{members.map(m=>{
        const c=last?.members?.find(c=>c.account_id===m.account_id), a=accounts.find(a=>a.id===m.account_id);
        const personal=totals?.find(t=>t.id===p?.id)?.members.find(x=>x.cycle_id===c?.id);
        const pending=!!p && (personal?.allocation_pending ?? a?.allocation_pending ?? true);
        const matchedCycle=c && a?.cycle?.id===c.id;
        const share=m.renewal_excluded?"已选择删除，不续费":!c?"下期生效":pending?"待账单拆分":!matchedCycle||a?.data_error?"—":money(personal?.amount_fen??a?.mine?.amount_fen??0);
        const multiplier=matchedCycle?a?.display?.cost_multiplier:null;
        return <div key={m.account_id}><div><strong>{a?.name||m.name}</strong><small>{p?"个人预计分摊":"个人分摊"} · <span className="amount">{share}</span></small></div><div className="vendor-member-stats"><small>原始消费</small><span className="amount">{matchedCycle&&a?.total_consumption!=null?consumption(a.total_consumption):"—"}</span></div><div className="vendor-member-stats"><small>{p?"预估倍率":"结算倍率"}</small><span className="amount">{multiplier?`${multiplier}×`:"—"}</span></div>{c&&<button className="text-button" onClick={()=>onDetail(c.id)}>明细</button>}</div>;
      })}</div>
      {admin&&<div className="row-actions vendor-actions"><button className="text-button" onClick={()=>onManage(v.id)}>管理账号组</button>{!!last&&<button className="text-button" onClick={()=>onManage(v.id,"add")}>添加账号</button>}{p?<><button className="text-button" onClick={()=>onManage(v.id,"edit")}>修改组成本</button><button className="text-button" onClick={()=>onSettle(p.members?.map(c=>c.id)||[],false)}>整组结算</button><button className="text-button primary-text" onClick={()=>onSettle(p.members?.map(c=>c.id)||[],true)}>结算并续费</button></>:!!last&&<button className="text-button primary-text" onClick={()=>onManage(v.id,"renew")}>整组续费</button>}</div>}
    </details>;
  })}</div>;
}

export function VendorManager({vendors,accounts,catalog,close,refresh,initialVendorId,initialAction}:{vendors:Vendor[];accounts:Account[];catalog:Catalog;close:()=>void;refresh:()=>Promise<void>;initialVendorId?:number;initialAction?:GroupAction}) {
  const dialog=useRef<HTMLDialogElement>(null);
  useEffect(()=>{dialog.current?.showModal();return()=>dialog.current?.close();},[]);
  const [editing,setEditing]=useState<{vendor?:Vendor;period?:VendorPeriod;action:GroupAction}|null>(()=>initialAction?{vendor:vendors.find(v=>v.id===initialVendorId),period:vendors.find(v=>v.id===initialVendorId)?currentPeriod(vendors.find(v=>v.id===initialVendorId)!):undefined,action:initialAction}:null);
  const initial=vendors.find(v=>v.id===initialVendorId), initialPeriod=initial && (currentPeriod(initial)||initial.periods[0]);
  const [name,setName]=useState(initial?.name||"");
  const [cost,setCost]=useState(costInput(initialPeriod?.cost_fen??initial?.cost_fen??0));
  const [creditor,setCreditor]=useState(initialPeriod?.creditor_id||initial?.creditor_id||catalog.users[0]?.id||0);
  const [start,setStart]=useState(inputDate(initialPeriod?.start));const [originalStart,setOriginalStart]=useState(initialPeriod?.start||"");
  const [mode,setMode]=useState(initialPeriod?.start_mode||"exact");const [selected,setSelected]=useState<number[]>([]);const [search,setSearch]=useState("");
  const [busy,setBusy]=useState(false);const [error,setError]=useState("");
  const [voidCandidate,setVoidCandidate]=useState<VendorPeriod|null>(null);const [archiveCandidate,setArchiveCandidate]=useState<Vendor|null>(null);
  const action=editing?.action, period=editing?.period;
  const available=groupCandidates(accounts).filter(a=>!search||`${a.name} ${a.id}`.toLowerCase().includes(search.toLowerCase()));
  function edit(v?:Vendor,action:GroupAction="edit",p?:VendorPeriod){const current=p||v&&currentPeriod(v);const last=current||v?.periods[0];setEditing({vendor:v,period:current,action});setName(v?.name||"");setCost(costInput(last?.cost_fen??v?.cost_fen??0));setCreditor(last?.creditor_id||v?.creditor_id||catalog.users[0]?.id||0);setOriginalStart(current?.start||"");setStart(inputDate(current?.start));setMode(current?.start_mode||"exact");setSelected([]);setSearch("");setError("");}
  async function save(e:React.FormEvent){
    e.preventDefault();if(action!=="add"&&!amountPattern.test(cost)){setError("金额最多保留两位小数");return;}
    if((action==="create"||action==="add")&&!selected.length){setError("请选择待配置账号");return;}
    setBusy(true);setError("");try{
      const v=editing?.vendor;
      if(action==="edit"&&!period) throw new Error("当前没有开放账期，请使用整组续费入口");
      if(action==="add" && v) await api(`/vendors/${v.id}/members`,"POST",{version:v.version,period_version:period?.end?null:period?.version??null,account_ids:selected});
      else if(action==="renew" && v) await api(`/vendors/${v.id}/renew`,"POST",{version:v.version,period_version:v.periods[0]?.version,cost,creditor_id:creditor});
      else if(action==="edit" && period) await api(`/vendor-periods/${period.id}`,"PATCH",{version:period.version,cost,creditor_id:creditor,start:originalStart&&start===inputDate(originalStart)?originalStart:fromInput(start),start_mode:mode,members:period.members?.map(c=>({cycle_id:c.id,version:c.version})),detached:[]});
      else await api(v?`/vendors/${v.id}/periods`:"/vendors","POST",{...(v?{version:v.version}:{name}),cost,creditor_id:creditor,start:fromInput(start),start_mode:mode,account_ids:selected});
      await refresh();close();
    }catch(e){setError((e as Error).message);}finally{setBusy(false);}
  }
  async function archive(v:Vendor){setBusy(true);setError("");try{await api(`/vendors/${v.id}/archive`,"POST",{version:v.version});await refresh();setArchiveCandidate(null);}catch(e){setError((e as Error).message);}finally{setBusy(false);}}
  async function voidPeriod(p:VendorPeriod){setBusy(true);setError("");try{await api(`/vendor-periods/${p.id}/void`,"POST",{version:p.version});await refresh();setVoidCandidate(null);}catch(e){setError((e as Error).message);}finally{setBusy(false);}}
  const title=action==="create"?"新建账号组":action==="add"?"添加账号":action==="renew"?"整组续费":action==="edit"?"整组配置":"账号组管理";
  return <dialog ref={dialog} className="modal wide" onCancel={e=>{e.preventDefault();if(!busy)close();}}><header><h2>{title}</h2><button className="icon-button" aria-label="关闭" disabled={busy} onClick={close}><X size={19}/></button></header><div className="form">
    {error&&<p className="form-error" role="alert">{error}</p>}
    {voidCandidate&&<section className="allocation"><strong>作废 {voidCandidate.name} 的整组结算？</strong><p className="field-hint">本期全部待付款订单将一并作废，已有续费周期保留。</p><div className="row-actions"><button className="button" disabled={busy} onClick={()=>setVoidCandidate(null)}>取消</button><button className="button" disabled={busy} onClick={()=>voidPeriod(voidCandidate)}>确认整组作废</button></div></section>}
    {archiveCandidate&&<section className="allocation"><strong>归档 {archiveCandidate.name}？</strong><p className="field-hint">以下成员将恢复待配置，历史账目保留。</p><p>{archiveCandidate.current_members?.filter(a=>!a.account_deleted).map(a=>accounts.find(x=>x.id===a.account_id)?.name||a.name).join("、")||"无存续成员"}</p><div className="row-actions"><button className="button" disabled={busy} onClick={()=>setArchiveCandidate(null)}>取消</button><button className="button" disabled={busy} onClick={()=>archive(archiveCandidate)}>确认归档</button></div></section>}
    {!editing?<>{vendors.filter(v=>!v.archived_at&&(!initialVendorId||v.id===initialVendorId)).map(v=>{const p=currentPeriod(v);return <section className="allocation" key={v.id}><div className="allocation-head"><strong>{v.name}</strong><b className="amount">{money(p?.cost_fen??v.periods[0]?.cost_fen??v.cost_fen)}</b></div><div className="row-actions">{!!v.periods.length&&<button className="text-button" onClick={()=>edit(v,"add")}>添加账号</button>}{!v.periods.some(p=>p.status==="open")&&<><button className="text-button" onClick={()=>edit(v,v.periods.length?"renew":"create")}>{v.periods.length?"整组续费":"配置首期"}</button><button className="text-button" disabled={busy} onClick={()=>setArchiveCandidate(v)}>归档</button></>}</div>{v.periods.map(p=><div className="vendor-period-row" key={p.id}><span>{date(p.start)} · {p.status==="open"?"进行中":"已结算"} · {money(p.cost_fen)}</span>{p.status==="open"?<button className="text-button" onClick={()=>edit(v,"edit",p)}>整组配置</button>:<button className="text-button" disabled={busy} onClick={()=>setVoidCandidate(p)}>整组作废</button>}</div>)}</section>;})}</>:<form className="form" onSubmit={save}>
      {action==="create"&&!editing.vendor&&<label>账号组名称<input required maxLength={120} value={name} onChange={e=>setName(e.target.value)}/></label>}
      {editing.vendor&&<strong>{editing.vendor.name}</strong>}
      {action!=="add"&&<div className="form-columns"><label>{action==="renew"?"下期准确总成本（元）":"本期准确总成本（元）"}<input required inputMode="decimal" value={cost} onChange={e=>setCost(e.target.value)}/></label><label>统一垫付人<Select value={creditor} onChange={e=>setCreditor(Number(e.target.value))}>{catalog.users.map(u=><option key={u.id} value={u.id}>{u.name}</option>)}</Select></label></div>}
      {(action==="create"||action==="edit")&&<div className="form-columns"><label>共同起点（北京时间）<input required type="datetime-local" step="1" value={start} onChange={e=>setStart(e.target.value)}/></label><label>起点方式<Select value={mode} onChange={e=>setMode(e.target.value as typeof mode)}><option value="exact">准确时间</option><option value="minute_estimate">按分钟估算</option>{mode==="date_estimate"&&<option value="date_estimate">按日期估算</option>}</Select></label></div>}
      {(action==="create"||action==="add")&&<fieldset className="member-picker"><legend>待配置账号 · 已选 {selected.length}</legend><input type="search" aria-label="搜索待配置账号" placeholder="搜索待配置账号" value={search} onChange={e=>setSearch(e.target.value)}/><div className="group-candidate-list">{available.map(a=><label className="checkline" key={a.id}><input type="checkbox" checked={selected.includes(a.id)} onChange={e=>setSelected(prev=>e.target.checked?[...prev,a.id]:prev.filter(id=>id!==a.id))}/>{a.name}</label>)}{!available.length&&<p className="field-hint">暂无可选的待配置账号</p>}</div></fieldset>}
      {action==="add"&&<p className="field-hint">{period?`沿用共同起点 ${date(period.start)}，保存后需重新确认整组账单。`:"新增成员从下一期生效，已结算账期保持不变。"}</p>}
      {action==="edit"&&<><div className="group-member-names">{period?.members?.map(c=><p className="field-hint" key={c.id}>{c.current_name||c.name}{c.account_deleted?" · 已删除，保留本期账务":""}</p>)}</div><p className="field-hint">保存后整组账单确认失效。</p></>}
      {action==="renew"&&<p className="field-hint">从上一期结束时间 {date(editing.vendor?.periods[0]?.end||"")} 连续开始，自动沿用全组存续成员。</p>}
      <footer><button type="button" className="button" disabled={busy} onClick={close}>取消</button><button className="button primary" disabled={busy}>{busy?"保存中…":action==="renew"?"确认整组续费":"保存"}</button></footer>
    </form>}
  </div></dialog>;
}

export function VendorBillPanel({period,cycles,end,previewKey,onConfirmed,onDraftChange}:{period:VendorPeriod;cycles:Cycle[];end:string;previewKey:string;onConfirmed:()=>Promise<void>;onDraftChange?:(dirty:boolean)=>void}){
  const [csv,setCsv]=useState("");const [fileName,setFileName]=useState("");const [preview,setPreview]=useState<VendorBillPreview|null>(null);
  const [difference,setDifference]=useState(false);const [outside,setOutside]=useState(false);const [busy,setBusy]=useState(false);const [error,setError]=useState("");
  const [start,setStart]=useState(inputDate(period.start));const seq=useRef(0);const reads=useRef(0);const manual=useManualBilling(period.manual_basis);
  function invalidate(){++seq.current;setPreview(null);setDifference(false);setOutside(false);setBusy(false);}
  function changed(){invalidate();onDraftChange?.(true);}
  const manualWindow=useRef(end);
  useEffect(()=>{invalidate();manual.clearDiagnostics(manualWindow.current!==end);manualWindow.current=end;},[end,period.version,previewKey]);
  useEffect(()=>()=>{++seq.current;++reads.current;},[]);
  async function inspect(existing=false){
    if(!manual.valid){setError("倍率须为非负数，最多六位小数");return;}
    const token=++seq.current;setBusy(true);setError("");
    try{
      const p=await api<VendorBillPreview>(`/vendor-periods/${period.id}/bill-previews`,"POST",{version:period.version,end,
        ...(manual.source==="local_only"?{}:existing?{import_id:period.bill_import_id}:{csv,file_name:fileName}),...(manual.basis?{manual_basis:manual.basis}:{})});
      if(token!==seq.current)return;setPreview(p);manual.setDiagnostics(p);setDifference(false);setOutside(false);
    }catch(e){if(token===seq.current){setPreview(null);setError((e as Error).message);}}finally{if(token===seq.current)setBusy(false);}
  }
  async function confirm(){if(!preview)return;const token=seq.current;setBusy(true);setError("");try{
    await api(`/vendor-periods/${period.id}/bill-confirm`,"POST",{version:period.version,preview_id:preview.id,accept_difference:difference,accept_outside:outside});
    if(token===seq.current){setPreview(null);onDraftChange?.(false);await onConfirmed();}
  }catch(e){if(token===seq.current)setError((e as Error).message);}finally{if(token===seq.current)setBusy(false);}}
  async function confirmStart(){setBusy(true);setError("");try{await api(`/vendor-periods/${period.id}/start-confirm`,"POST",{version:period.version,start:start===inputDate(period.start)?period.start:fromInput(start),members:cycles.map(c=>({cycle_id:c.id,version:c.version}))});await onConfirmed();}catch(e){setError((e as Error).message);}finally{setBusy(false);}}
  return <section className="bill-import vendor-bill"><div className="bill-import-head"><strong>{period.name} · 整组上游账单</strong><b className="amount">{money(period.cost_fen)}</b></div><p className="field-hint">{cycles.map(c=>c.name).join("、")} · 统一按本期总成本分摊</p>
    {!period.start_confirmed&&<div className="start-confirm"><label>确认共同起点<input type="datetime-local" step="1" value={start} onChange={e=>setStart(e.target.value)}/></label><button className="button" disabled={busy} onClick={confirmStart}>确认起点</button></div>}
    <ManualBillingEditor disabled={busy&&!!preview} manual={manual} accounts={cycles} changed={changed}/>
    {manual.source!=="local_only"&&<label>CSV 文件<input type="file" accept=".csv,text/csv" onChange={async e=>{const f=e.target.files?.[0];if(!f)return;if(f.size>10*1024*1024){setError("CSV 不能超过 10 MB");return;}const token=++reads.current;changed();manual.clearDiagnostics();const text=await f.text();if(token===reads.current){setCsv(text);setFileName(f.name);}}}/></label>}
    <div className="start-actions"><button className="button" disabled={busy||!manual.valid||(manual.source!=="local_only"&&!csv.trim())||!end} onClick={()=>inspect()}>{busy?"处理中…":"账单预览"}</button>{manual.source!=="local_only"&&!!period.bill_import_id&&<button className="button" disabled={busy||!manual.valid||!end} onClick={()=>inspect(true)}>沿用已导入账单</button>}</div>
    {preview&&<div className="bill-stats">
      <div><span>组准确成本</span><b>{money(preview.cost_fen)}</b></div><div><span>账单总额</span><b>{preview.bill_total_fen==null?"未提供":money(preview.bill_total_fen)}</b></div>
      {preview.bill_total_fen!=null&&<div><span>差额</span><b>{preview.difference_amount&&Number(preview.difference_amount)!==0&&Math.abs(Number(preview.difference_amount))<0.005?"差异不足 ¥0.01":money(preview.cost_fen-preview.bill_total_fen)}</b></div>}
      {preview.manual_summary&&<div><span>手动补计权重</span><b>{billedMoney(preview.manual_summary.amount)}</b></div>}
      <div><span>未匹配上游</span><b>{preview.unmatched_count} 条 · {billedMoney(preview.unmatched_amount)}</b></div>
      <p className="field-hint">{preview.basis==="manual_supplement"?"按已匹配扣费与手动补计权重拆分，组总成本不变。":preview.basis==="local_consumption"?"无正金额匹配，按各账号本地原始消费分摊。":"未匹配费用按各账号已匹配上游扣费比例分摊。"}</p>
      {preview.coverage_warnings.map(g=><p key={g.account_id} className="form-error">{g.warning}</p>)}
      {preview.accounts.map(a=><div key={a.account_id}><span>{a.name}</span><b className="amount">{a.amount_fen==null?"—":money(a.amount_fen)}</b></div>)}
      {!!preview.outside_count&&<div><span>范围外</span><b>{preview.outside_count} 条 · {billedMoney(preview.outside_amount)}</b></div>}
      {(preview.unmatched_count>0||preview.outside_count>0||!!preview.local_unmatched?.length)&&<button className="text-button" onClick={()=>downloadCsv(`/vendor-periods/${period.id}/bill-previews/${preview.id}/unmatched.csv`,"账号组未匹配账单.csv").catch(e=>setError(e.message))}>导出未匹配明细</button>}
      {preview.difference_required&&<label className="checkline"><input type="checkbox" checked={difference} onChange={e=>setDifference(e.target.checked)}/>确认组成本与账单、未匹配和覆盖差异</label>}
      {!!preview.outside_count&&<label className="checkline"><input type="checkbox" checked={outside} onChange={e=>setOutside(e.target.checked)}/>确认排除范围外记录</label>}
      {!preview.can_confirm&&<p className="form-error">{preview.manual_basis?"请确认本地消费覆盖及正金额分摊依据":"没有正金额匹配或本地消费依据，无法分摊"}</p>}
      <button className="button primary" disabled={busy||!preview.can_confirm||!period.start_confirmed||(preview.difference_required&&!difference)||(preview.outside_count>0&&!outside)} onClick={confirm}>确认分摊依据</button>
    </div>}
    {error&&<p className="form-error" role="alert">{error}</p>}
  </section>;
}
