import { useEffect, useRef, useState } from "react";
import { api } from "./api";

export type DeletionScopeValue = {account_id:number;digest:string;targets:{id:number;name:string;shadow:boolean}[]};
export function DeletionTargets({value}:{value:DeletionScopeValue}) {
  return <ul className="deletion-targets">{value.targets.map(target=><li key={target.id}><span>{target.name}</span><small>#{target.id} · {target.shadow?"影子账号":"母账号"}</small></li>)}</ul>;
}
export function DeletionScope({accountId,onConfirmed}:{accountId:number;onConfirmed:(value:DeletionScopeValue|null)=>void}) {
  const [value,setValue]=useState<DeletionScopeValue|null>(null);const [error,setError]=useState("");const [generation,setGeneration]=useState(0);const callback=useRef(onConfirmed);callback.current=onConfirmed;
  const [checked,setChecked]=useState(false);
  useEffect(()=>{let active=true;setValue(null);setChecked(false);setError("");callback.current(null);
    api<DeletionScopeValue>(`/accounts/${accountId}/deletion-scope`).then(result=>{if(active)setValue(result);}).catch(e=>{if(active)setError(e.message);});
    return()=>{active=false;};
  },[accountId,generation]);
  return <div className="deletion-scope">
    <strong>将删除以下账号</strong>
    {value?<><DeletionTargets value={value}/><label className="checkline"><input type="checkbox" checked={checked} onChange={e=>{setChecked(e.target.checked);callback.current(e.target.checked?value:null);}}/>确认删除以上 {value.targets.length} 个账号</label><button className="text-button" onClick={()=>setGeneration(x=>x+1)}>重新核验范围</button></>:error?<><p className="form-error" role="alert">{error}</p><button className="text-button" onClick={()=>setGeneration(x=>x+1)}>重试核验</button></>:<p className="muted" role="status">正在核验删除范围…</p>}
    <p className="field-hint">出单后执行删除，历史账单保留。</p>
  </div>;
}
