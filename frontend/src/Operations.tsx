import { Select } from "./Select";
import { useEffect, useRef, useState } from "react";
import { api, date, downloadCsv, fromInput, money } from "./api";

const actions: Record<string,string> = {
  add_vendor_members:"追加账号组成员", renew_vendor:"整组续费",
  save_vendor:"保存账号组默认设置", archive_vendor:"归档账号组", save_vendor_period:"保存账号组整组配置",
  confirm_vendor_start:"确认账号组起点", confirm_vendor_bill:"确认账号组账单依据", settle_vendor:"账号组整组出单", void_vendor:"账号组整组作废", reset_estimated:"撤销旧待确认成本配置",
  create_cycle:"创建周期", edit_cycle:"修改成本与周期", confirm_cycle_start:"确认周期起点",
  mark_free:"标记免费", upgrade_paid:"升级付费", settle:"结算出单", create_expense:"新增固定费用",
  edit_expense:"修改固定费用", settle_expense:"固定费用出单", void_expense:"作废固定费用结算",
  import_bill:"确认上游账单", confirm_manual_basis:"确认补计依据", confirm_local_billing:"确认本地计费依据", bill_allocation:"确认账单分摊方式", request_account_deletion:"结算后删除账号",
  retry_account_deletion:"重试删除账号", cancel_account_deletion:"取消删除账号", account_deletion_done:"账号已删除",
  account_deletion_failed:"账号删除未完成", account_deletion_verify:"账号删除待核验", merge:"合并订单",
  submit:"付款方结清", mark_paid:"管理员标记结清", netting_payment:"对冲结清", confirm:"管理员标记结清",
  reject:"退回付款", split:"拆分订单", void:"作废结算",
};
type Audit = { id:number; actor_name:string; action:string; entity_id:number; created_at:string; payload: Record<string,any> };
type AuditPage = {items:Audit[];next_before:number|null};
type Status = {last_sync:string|null;sync_error:string|null;accounts:{checked_at:string|null;error:string|null};pending_deletions:number;checkpoint:null|{mode:string;watermark:number;last_reconciled:string}};

export function OperationStatus() {
  const [status,setStatus] = useState<Status|null>(null);
  const [error,setError] = useState("");
  useEffect(() => {
    let active=true;
    async function load(){if(document.visibilityState==="hidden")return;try{const value=await api<Status>("/operations/status");if(active){setStatus(value);setError("");}}catch(e){if(active)setError((e as Error).message);}}
    void load();const timer=setInterval(()=>void load(),15000);document.addEventListener("visibilitychange",load);
    return()=>{active=false;clearInterval(timer);document.removeEventListener("visibilitychange",load);};
  },[]);
  return <section className="operation-status" aria-label="同步状态">
    <div><span>消费同步</span><b>{status?.sync_error ? "同步异常" : status?.last_sync ? date(status.last_sync) : "尚未同步"}</b></div>
    <div><span>账号核对</span><b>{status?.accounts.error ? "核对异常" : status?.accounts.checked_at ? date(status.accounts.checked_at) : "尚未核对"}</b></div>
    <div><span>待处理删除</span><b>{status?.pending_deletions ?? "—"}</b></div>
    {status?.checkpoint && <div><span>全量对账</span><b>{date(status.checkpoint.last_reconciled)}</b></div>}
    {(error || status?.sync_error || status?.accounts.error) && <p className="form-error" role="alert">{error || status?.sync_error || status?.accounts.error}</p>}
  </section>;
}

export function AuditLog() {
  const [action,setAction]=useState("");const [from,setFrom]=useState("");const [to,setTo]=useState("");
  const [page,setPage]=useState<AuditPage>({items:[],next_before:null});const [error,setError]=useState("");const [busy,setBusy]=useState(false);
  const generation=useRef(0);const query=new URLSearchParams();if(action)query.set("action",action);
  let invalid=false;
  try{if(from)query.set("from",fromInput(from));if(to)query.set("to",fromInput(to));if(from&&to&&from>=to)invalid=true;}catch{invalid=true;}
  const search=query.toString();
  async function load(before?:number){
    const ticket=++generation.current;setBusy(true);setError("");
    try{const value=await api<AuditPage>(`/audit?${search}${before?`&before=${before}`:""}`);if(ticket===generation.current)setPage(old=>before?{...value,items:[...old.items,...value.items]}:value);}
    catch(e){if(ticket===generation.current)setError((e as Error).message);}finally{if(ticket===generation.current)setBusy(false);}
  }
  useEffect(()=>{setPage({items:[],next_before:null});if(invalid){setError("请检查起止时间");setBusy(false);return;}void load();return()=>{generation.current++;};},[search,invalid]);
  async function download(){setBusy(true);setError("");try{await downloadCsv(`/audit.csv?${search}`,"settlement-audit.csv");}catch(e){setError((e as Error).message);}finally{setBusy(false);}}
  return <section aria-label="操作记录">
    <div className="audit-filters">
      <label>操作<Select value={action} onChange={e=>setAction(e.target.value)}><option value="">全部操作</option>{Object.entries(actions).map(([key,label])=><option key={key} value={key}>{label}</option>)}</Select></label>
      <label>开始时间<input type="datetime-local" value={from} onChange={e=>setFrom(e.target.value)}/></label>
      <label>结束时间<input type="datetime-local" value={to} onChange={e=>setTo(e.target.value)}/></label>
      <button className="button" disabled={busy||invalid} onClick={download}>导出 CSV</button>
    </div>
    {error&&<p className="form-error" role="alert">{error}</p>}
    <div className="audit-list">{page.items.map(a=><div key={a.id}><div><strong>{actions[a.action]||a.action}</strong><span>{a.actor_name} · #{a.entity_id}</span>{a.action==="edit_cycle"&&a.payload.before&&a.payload.after&&<b>{money(a.payload.before.cost_fen)} → {money(a.payload.after.cost_fen)}</b>}<details><summary>记录内容</summary><pre>{JSON.stringify(a.payload,null,2)}</pre></details></div><time>{date(a.created_at)}</time></div>)}</div>
    {!page.items.length&&!busy&&!error&&<div className="empty">暂无操作记录</div>}
    {page.next_before&&<button className="button" disabled={busy} onClick={()=>load(page.next_before!)}>加载更多</button>}
    {busy&&<p className="muted" role="status">正在读取…</p>}
  </section>;
}
