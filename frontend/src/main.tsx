import React, { useEffect, useRef, useState } from "react";
import { createRoot } from "react-dom/client";
import {
  ArrowDownLeft,
  ArrowUpRight,
  Bell,
  ChevronRight,
  CircleAlert,
  CircleCheck,
  Clock3,
  Edit3,
  Layers3,
  Plus,
  ReceiptText,
  Search,
  Settings2,
  Wallet,
  X,
} from "lucide-react";
import {
  api,
  downloadCsv,
  ApiError,
  setCsrf,
  money,
  consumption,
  costInput,
  date,
  inputDate,
  fromInput,
  amountPattern,
  billedMoney,
  canImportBill,
  canUpgradePaid,
  isOperationalAccount,
} from "./api";
import type {
  Actor,
  Account,
  Catalog,
  Cycle,
  Overview,
  Order,
  Preview,
  Calculation,
  Expense,
  ExpenseCalculation,
  BillPreview,
  NetBalance,
  NetPayment,
  StartMode,
  CostKind,
  Share,
  BillMode,
  DeletionJob,
} from "./types";
import { useAutoPreview } from "./useAutoPreview";
import { useAccountState } from "./useAccountState";
import { Select } from "./Select";
import { NettingList, NettingReceivables, NettingDetails } from "./Netting";
import { AuditLog, OperationStatus } from "./Operations";
import { DeletionScope, DeletionTargets, type DeletionScopeValue } from "./DeletionScope";
import "./style.css";
import settlementIcon from "./assets/settlement.svg?raw";

const statusName: Record<string, string> = {
  pending: "待付款",
  submitted: "待管理员处理",
  paid: "已结清",
  void: "已作废",
  open: "进行中",
  settled: "已结算",
};
const statusIcons: Record<string, typeof Clock3> = {
  pending: Clock3,
  submitted: ArrowUpRight,
  paid: CircleCheck,
  void: X,
  open: Clock3,
  settled: CircleCheck,
};
function Status({ value }: { value: string }) {
  const Icon = statusIcons[value] || Clock3;
  return (
    <span className={"status " + value}>
      <Icon size={12} />
      {statusName[value] || value}
    </span>
  );
}
function Empty({ text }: { text: string }) {
  return (
    <div className="empty">
      <ReceiptText size={30} strokeWidth={1.3} />
      <span>{text}</span>
    </div>
  );
}
function Modal({
  title,
  children,
  close,
  wide = false,
}: {
  title: string;
  children: React.ReactNode;
  close: () => void;
  wide?: boolean;
}) {
  const ref = useRef<HTMLDialogElement>(null);
  useEffect(() => {
    ref.current?.showModal();
    const el = ref.current;
    return () => el?.close();
  }, []);
  return (
    <dialog
      ref={ref}
      className={"modal " + (wide ? "wide" : "")}
      onCancel={(e) => {
        e.preventDefault();
        close();
      }}
      onClick={(e) => {
        if (e.target === e.currentTarget) close();
      }}
    >
      <header>
        <h2>{title}</h2>
        <button className="icon-button" onClick={close} aria-label="关闭">
          <X size={19} />
        </button>
      </header>
      {children}
    </dialog>
  );
}
function FormError({ error }: { error: string }) {
  return error ? (
    <div className="form-error" role="alert">
      <CircleAlert size={16} />
      {error}
    </div>
  ) : null;
}

function CycleForm({
  account,
  cycle,
  catalog,
  done,
  close,
}: {
  account?: Account;
  cycle?: Cycle;
  catalog: Catalog;
  done: () => void;
  close: () => void;
}) {
  const [aid] = useState(
    account?.id ||
      cycle?.account_id ||
      catalog.accounts.find(isOperationalAccount)?.id ||
      0,
  );
  const name = catalog.accounts.find(a => a.id === aid)?.name || account?.name || cycle?.name || "";
  const [cost, setCost] = useState(cycle ? costInput(cycle.cost_fen) : "");
  const [costKind, setCostKind] = useState<CostKind>(cycle?.cost_kind || "exact");
  const [creditor, setCreditor] = useState(
    cycle?.creditor_id || catalog.users[0]?.id || 0,
  );
  const initialStart =
    cycle?.start || account?.cycle?.end || new Date().toISOString();
  const [start, setStart] = useState(inputDate(initialStart));
  const [startMode, setStartMode] = useState<StartMode>(cycle?.start_mode || "exact");
  const [free, setFree] = useState(!cycle && account?.kind === "free");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");
  async function save(e: React.FormEvent) {
    e.preventDefault();
    if (!free && !amountPattern.test(cost)) {
      setError("金额最多保留两位小数");
      return;
    }
    setBusy(true);
    setError("");
    try {
      const body = {
        account_id: aid,
        name: name.trim(),
        cost,
        cost_kind: costKind,
        creditor_id: creditor,
        start: startMode === "earliest_usage" ? null : cycle && startMode === cycle.start_mode && start === inputDate(cycle.start) ? cycle.start : fromInput(startMode === "minute_estimate" ? start.slice(0, 16) : start),
        start_mode: startMode,
      };
      if (free)
        await api(`/accounts/${aid}/free`, "PUT", {
          name: name.trim(),
          version: account?.profile_version ?? null,
        });
      else if (cycle)
        await api(`/cycles/${cycle.id}`, "PATCH", {
          ...body,
          version: cycle.version,
        });
      else await api("/cycles", "POST", body);
      done();
    } catch (e) {
      setError((e as Error).message);
    } finally {
      setBusy(false);
    }
  }
  return (
    <Modal
      title={cycle ? "修改周期成本" : "配置账号"}
      close={() => !busy && close()}
    >
      <form onSubmit={save} className="form">
        <FormError error={error} />
        <label>
          账号
          <input value={name} readOnly />
        </label>
        {!cycle && (
          <label className="checkline">
            <input
              type="checkbox"
              checked={free}
              onChange={(e) => setFree(e.target.checked)}
            />
            免费资源
          </label>
        )}
        {!free && (
          <>
            <div className="form-columns">
              <label>
                成本（元）
                <input
                  required
                  inputMode="decimal"
                  placeholder="0.00"
                  value={cost}
                  onChange={(e) => setCost(e.target.value)}
                  onBlur={() =>
                    amountPattern.test(cost) && setCost(Number(cost).toFixed(2))
                  }
                />
              </label>
              <label>
                垫付人
                <Select
                  value={creditor}
                  onChange={(e) => setCreditor(Number(e.target.value))}
                >
                  {cycle &&
                    !catalog.users.some((u) => u.id === cycle.creditor_id) && (
                      <option value={cycle.creditor_id}>
                        {account?.creditor_name ||
                          cycle.creditor_name ||
                          `用户 #${cycle.creditor_id}`}
                        （历史用户）
                      </option>
                    )}
                  {catalog.users.map((u) => (
                    <option key={u.id} value={u.id}>
                      {u.name}
                    </option>
                  ))}
                </Select>
              </label>
            </div>
            <label>成本类型<Select value={costKind} onChange={(e) => setCostKind(e.target.value as CostKind)}><option value="exact">准确成本</option><option value="estimated">模糊成本</option></Select></label>
            {cycle?.cost_source === "upstream_bill" && cost !== costInput(cycle.cost_fen) && <p className="field-hint">修改金额将改用手动成本分摊，原账单保留。</p>}
            <label>
              起点方式
              <Select value={startMode} onChange={(e) => setStartMode(e.target.value as typeof startMode)}>
                <option value="exact">精确时间</option>
                <option value="minute_estimate">按时间估算</option>
                {cycle?.start_mode === "date_estimate" && <option value="date_estimate">按日期估算</option>}
                <option value="earliest_usage">从最早使用开始</option>
              </Select>
            </label>
            {startMode !== "earliest_usage" ? (
              <label>
                {startMode === "date_estimate" ? "周期日期（北京时间）" : "周期开始（北京时间）"}
                <input type={startMode === "date_estimate" ? "date" : "datetime-local"} step={startMode === "minute_estimate" ? 60 : 1} required value={startMode === "date_estimate" ? start.slice(0, 10) : startMode === "minute_estimate" ? start.slice(0, 16) : start} onChange={(e) => setStart(startMode === "date_estimate" ? `${e.target.value}T00:00:00` : e.target.value)} />
              </label>
            ) : (
              <p className="field-hint">最早已采集使用：{date(catalog.accounts.find((a) => a.id === aid)?.earliest_usage_at)}</p>
            )}
            {startMode !== "earliest_usage" && <p className="field-hint">可用记录起点：{date(catalog.coverage_from)}</p>}
          </>
        )}
        <footer>
          <button
            type="button"
            className="button"
            disabled={busy}
            onClick={close}
          >
            取消
          </button>
          <button className="button primary" disabled={busy}>
            {busy ? "保存中…" : "保存"}
          </button>
        </footer>
      </form>
    </Modal>
  );
}

function UpgradeForm({
  account,
  catalog,
  done,
  close,
}: {
  account: Account;
  catalog: Catalog;
  done: () => void;
  close: () => void;
}) {
  const name = catalog.accounts.find(a => a.id === account.id)?.name || account.name;
  const [cost, setCost] = useState("");
  const [costKind, setCostKind] = useState<CostKind>("exact");
  const [creditor, setCreditor] = useState(catalog.users[0]?.id || 0);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");
  async function save(e: React.FormEvent) {
    e.preventDefault();
    if (!amountPattern.test(cost)) {
      setError("金额最多保留两位小数");
      return;
    }
    setBusy(true);
    setError("");
    try {
      await api(`/accounts/${account.id}/upgrade-paid`, "POST", {
        name: name.trim(),
        cost,
        cost_kind: costKind,
        creditor_id: creditor,
        version: account.profile_version,
      });
      done();
    } catch (e) {
      setError((e as Error).message);
    } finally {
      setBusy(false);
    }
  }
  return (
    <Modal title="升级付费" close={() => !busy && close()}>
      <form onSubmit={save} className="form">
        <FormError error={error} />
        <label>
          账号
          <input value={name} readOnly />
        </label>
        <div className="form-columns">
          <label>
            成本（元）
            <input required inputMode="decimal" placeholder="0.00" value={cost} onChange={(e) => setCost(e.target.value)} onBlur={() => amountPattern.test(cost) && setCost(Number(cost).toFixed(2))} />
          </label>
          <label>
            垫付人
            <Select value={creditor} onChange={(e) => setCreditor(Number(e.target.value))}>
              {catalog.users.map((u) => <option key={u.id} value={u.id}>{u.name}</option>)}
            </Select>
          </label>
        </div>
        <label>
          成本类型
          <Select value={costKind} onChange={(e) => setCostKind(e.target.value as CostKind)}>
            <option value="exact">准确成本</option>
            <option value="estimated">模糊成本</option>
          </Select>
        </label>
        <p className="field-hint">保存时开始计费，升级前用量不计入。</p>
        <footer>
          <button type="button" className="button" disabled={busy} onClick={close}>取消</button>
          <button className="button primary" disabled={busy || !account.profile_version}>
            {busy ? "保存中…" : "升级付费"}
          </button>
        </footer>
      </form>
    </Modal>
  );
}

function ExpenseForm({
  expense,
  catalog,
  done,
  close,
}: {
  expense?: ExpenseCalculation;
  catalog: Catalog;
  done: () => void;
  close: () => void;
}) {
  const source = expense?.expense;
  const [name, setName] = useState(source?.name || "");
  const [cost, setCost] = useState(source ? costInput(source.cost_fen) : "");
  const [incurredOn, setIncurredOn] = useState(
    source?.incurred_on || inputDate().slice(0, 10),
  );
  const [creditor, setCreditor] = useState(
    source?.creditor_id || catalog.users[0]?.id || 0,
  );
  const [mode, setMode] = useState<"equal" | "custom">(
    source?.allocation_mode || "equal",
  );
  const [members, setMembers] = useState<Record<number, string>>(() =>
    Object.fromEntries(
      (source?.members || []).map((m) => [
        m.user_id,
        m.percent || (m.weight !== undefined ? (Number(m.weight) / 100).toFixed(2) : ""),
      ]),
    ),
  );
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");
  const availableUsers = [...catalog.users, ...(source?.members || []).filter((m) => !catalog.users.some((u) => u.id === m.user_id)).map((m) => ({id: m.user_id, name: expense?.allocations.find((a) => a.user_id === m.user_id)?.user_name || `用户 #${m.user_id}`}))];
  const selected = availableUsers.filter((u) => Object.prototype.hasOwnProperty.call(members, u.id));
  const validRatios = selected.every((u) => /^(0|[1-9][0-9]{0,2})(\.[0-9]{1,2})?$/.test(members[u.id]));
  const totalBps = selected.reduce((sum, u) => sum + Math.round(Number(members[u.id] || 0) * 100), 0);
  const total = totalBps / 100;
  async function save(e: React.FormEvent) {
    e.preventDefault();
    if (!amountPattern.test(cost) || Number(cost) <= 0) {
      setError("金额最多保留两位小数，且必须大于零");
      return;
    }
    if (!selected.length) {
      setError("请选择参与人");
      return;
    }
    if (mode === "custom" && (!validRatios || totalBps !== 10000)) {
      setError("比例最多两位小数，合计须为 100.00%");
      return;
    }
    setBusy(true);
    setError("");
    try {
      const body = {
        name: name.trim(),
        cost,
        incurred_on: incurredOn,
        creditor_id: creditor,
        allocation_mode: mode,
        members: selected.map((u) => ({
          user_id: u.id,
          ...(mode === "custom" ? { percent: Number(members[u.id] || 0).toFixed(2) } : {}),
        })),
      };
      if (source) await api(`/expenses/${source.id}`, "PATCH", { ...body, version: source.version });
      else await api("/expenses", "POST", body);
      done();
    } catch (e) {
      setError((e as Error).message);
    } finally {
      setBusy(false);
    }
  }
  return (
    <Modal title={source ? "修改固定费用" : "新增固定费用"} close={() => !busy && close()}>
      <form className="form" onSubmit={save}>
        <FormError error={error} />
        <label>
          名称
          <input required maxLength={120} value={name} onChange={(e) => setName(e.target.value)} />
        </label>
        <div className="form-columns">
          <label>
            金额（元）
            <input required inputMode="decimal" placeholder="0.00" value={cost} onChange={(e) => setCost(e.target.value)} onBlur={() => amountPattern.test(cost) && setCost(Number(cost).toFixed(2))} />
          </label>
          <label>
            日期
            <input required type="date" value={incurredOn} onChange={(e) => setIncurredOn(e.target.value)} />
          </label>
        </div>
        <label>
          垫付人
          <Select value={creditor} onChange={(e) => setCreditor(Number(e.target.value))}>
            {source && !catalog.users.some((u) => u.id === source.creditor_id) && <option value={source.creditor_id}>{expense?.creditor_name}（历史用户）</option>}
            {catalog.users.map((u) => <option key={u.id} value={u.id}>{u.name}</option>)}
          </Select>
        </label>
        <fieldset className="member-picker">
          <legend>参与人</legend>
          <div className="member-list">
            {availableUsers.map((u) => {
              const checked = Object.prototype.hasOwnProperty.call(members, u.id);
              return (
                <div key={u.id} className="member-row">
                  <label className="checkline">
                  <input type="checkbox" checked={checked} onChange={(e) => setMembers((prev) => {
                    const next = { ...prev };
                    if (e.target.checked) next[u.id] = mode === "custom" ? "0.00" : "";
                    else delete next[u.id];
                    return next;
                  })} />
                  <span>{u.name}</span>
                  </label>
                  {mode === "custom" && checked && <input className="percent-input" inputMode="decimal" aria-label={`${u.name}比例`} value={members[u.id]} onChange={(e) => setMembers((prev) => ({ ...prev, [u.id]: e.target.value }))} />}
                  {mode === "custom" && checked && <span>%</span>}
                </div>
              );
            })}
          </div>
        </fieldset>
        <div className="segments allocation-mode" role="group" aria-label="分摊方式">
          <button type="button" className={mode === "equal" ? "active" : ""} onClick={() => setMode("equal")}>等额</button>
          <button type="button" className={mode === "custom" ? "active" : ""} onClick={() => {
            if (mode === "equal" && selected.length) setMembers(Object.fromEntries(selected.map((u, i) => [u.id, ((Math.floor(10000 / selected.length) + (i < 10000 % selected.length ? 1 : 0)) / 100).toFixed(2)])));
            setMode("custom");
          }}>自定义比例</button>
        </div>
        {mode === "custom" && <p className={Math.abs(total - 100) < 0.001 ? "field-hint valid-hint" : "field-hint"}>合计 {total.toFixed(2)}%</p>}
        <footer>
          <button type="button" className="button" disabled={busy} onClick={close}>取消</button>
          <button className="button primary" disabled={busy}>{busy ? "保存中…" : "保存"}</button>
        </footer>
      </form>
    </Modal>
  );
}

function Allocation({ value }: { value: Calculation | ExpenseCalculation }) {
  const isExpense = "source_type" in value && value.source_type === "fixed_expense";
  const expenseValue = value as ExpenseCalculation;
  const cycleValue = value as Calculation;
  const title = isExpense ? expenseValue.expense.name : cycleValue.cycle.name;
  const amount = isExpense ? expenseValue.expense.cost_fen : cycleValue.bill?.billable_cost_fen ?? cycleValue.cycle.cost_fen;
  const period = isExpense
    ? expenseValue.expense.incurred_on
    : `${date(cycleValue.cycle.start)} — ${date(cycleValue.end)}`;
  return (
    <section className="allocation">
      <div className="allocation-head">
        <div>
          <strong>{title}</strong>
          <span>{period}</span>
        </div>
        <strong className="amount">{money(amount)}</strong>
      </div>
      {!isExpense && <div className="detail-facts"><span>周期总消费 <b>{consumption(cycleValue.total_consumption)}</b></span><span>调用 <b>{cycleValue.record_count.toLocaleString()}</b></span><span title="本次计费（元）÷ 周期原始总消费">{cycleValue.display?.settled ? "结算倍率" : "预估倍率"} <b>{cycleValue.display?.cost_multiplier != null ? `${cycleValue.display.cost_multiplier}×` : "—"}</b></span></div>}
      {!isExpense && cycleValue.bill && <div className="detail-facts"><span>{cycleValue.bill.allocation_mode === "matched_only" ? "精准匹配" : "上游扣费分摊"}</span><span>上游总扣费 <b>{money(cycleValue.cycle.cost_fen)}</b></span><span>本次计费 <b>{money(amount)}</b></span><span>已匹配 <b>{cycleValue.bill.matched_count} 条</b></span><span>{cycleValue.bill.allocation_mode === "matched_only" ? "未匹配不计费" : "未匹配"} <b>{cycleValue.bill.unmatched_count} 条 · {billedMoney(cycleValue.bill.unmatched_amount)}</b></span></div>}
      <AllocationRows allocations={value.allocations} />
    </section>
  );
}

function AllocationRows({ allocations }: { allocations: Share[] }) {
  return (
    <div className="allocation-list">
      {allocations.map((a) => (
        <div key={a.user_id}>
          <span>
            {a.user_name}
            {a.self_covered && <small className="self">自行承担</small>}
          </span>
          <span>{(Number(a.share) * 100).toFixed(2)}%</span>
          <b className="amount">{money(a.amount_fen)}</b>
        </div>
      ))}
    </div>
  );
}

function StartConfirmation({ calculation, onConfirmed }: { calculation: Calculation; onConfirmed: () => Promise<void> }) {
  const [value, setValue] = useState(inputDate(calculation.start_lower || calculation.cycle.start));
  const [candidate, setCandidate] = useState<{ version: number; anchor: string; usage: { id: number; created_at: string } | null } | null>(null);
  const [method, setMethod] = useState<"provided_time" | "first_usage_after">("provided_time");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");
  async function findCandidate() {
    setBusy(true); setError(""); setCandidate(null);
    try {
      setCandidate(await api(`/cycles/${calculation.cycle.id}/start-candidate?after=${encodeURIComponent(fromInput(value))}`));
    } catch (e) { setError((e as Error).message); }
    finally { setBusy(false); }
  }
  async function confirm() {
    setBusy(true); setError("");
    try {
      await api(`/cycles/${calculation.cycle.id}/start-confirm`, "POST", {
        version: method === "first_usage_after" ? candidate!.version : calculation.cycle.version,
        start: method === "first_usage_after" ? candidate!.anchor : fromInput(value),
        method,
        ...(method === "first_usage_after" ? { source_usage_id: candidate!.usage!.id } : {}),
      });
      await onConfirmed();
    } catch (e) { setError((e as Error).message); }
    finally { setBusy(false); }
  }
  return <div className="start-confirm">
    <div className="form-columns">
      <label>确认方式<Select value={method} disabled={busy} onChange={(e) => { setMethod(e.target.value as typeof method); setCandidate(null); }}>
        <option value="provided_time">直接填写准确时间</option>
        <option value="first_usage_after">充值后的首个请求</option>
      </Select></label>
      <label>{method === "first_usage_after" ? "充值时间（北京时间）" : "准确起点（北京时间）"}<input type="datetime-local" step="1" value={value} onChange={(e) => { setValue(e.target.value); setCandidate(null); }} /></label>
    </div>
    {candidate && <p className="candidate-result">{candidate.usage ? `首请求：${date(candidate.usage.created_at)}` : "充值时间后没有已采集请求"}</p>}
    <div className="start-actions">
      {method === "first_usage_after" && <button className="button" disabled={busy || !value} onClick={findCandidate}>查询首请求</button>}
      <button className="button primary" disabled={busy || !value || (method === "first_usage_after" && !candidate?.usage)} onClick={confirm}>{busy ? "处理中…" : "确认起点"}</button>
    </div>
    <FormError error={error} />
  </div>;
}

function BillAllocation({ cycle, preview }: { cycle: Cycle; preview: BillPreview }) {
  return (
    <section className="allocation bill-allocation">
      <div className="allocation-head">
        <div>
          <strong>{cycle.name}</strong>
          <span>上游账单分摊</span>
        </div>
        <strong className="amount">{money(preview.billable_cost_fen ?? preview.cost_fen)}</strong>
      </div>
      <AllocationRows allocations={preview.allocations} />
    </section>
  );
}

function BillImportPanel({
  cycle,
  calculation,
  end,
  previewKey,
  onConfirmed,
}: {
  cycle: Cycle;
  calculation?: Calculation;
  end: string;
  previewKey: string;
  onConfirmed: () => Promise<void>;
}) {
  const [csv, setCsv] = useState("");
  const [fileName, setFileName] = useState("");
  const [preview, setPreview] = useState<BillPreview | null>(null);
  const [acceptDifference, setAcceptDifference] = useState(false);
  const [acceptOutside, setAcceptOutside] = useState(false);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");
  const effectiveCycle = calculation && calculation.cycle.version >= cycle.version ? calculation.cycle : cycle;
  const [mode, setMode] = useState<BillMode>(effectiveCycle.bill_allocation_mode || "proportional");
  const requestVersion = useRef(0);
  const fileRead = useRef(0);

  useEffect(() => { setMode(effectiveCycle.bill_allocation_mode || "proportional"); }, [effectiveCycle.id, effectiveCycle.bill_allocation_mode]);

  useEffect(() => {
    ++requestVersion.current;
    setPreview(null);
    setBusy(false);
    setAcceptDifference(false);
    setAcceptOutside(false);
    setError("");
  }, [end, effectiveCycle.id, effectiveCycle.version, previewKey]);
  useEffect(() => () => { ++requestVersion.current; ++fileRead.current; }, []);

  async function inspect(useExisting = false, selectedMode = mode) {
    if (!end) { setError("请填写有效的结束时间"); return; }
    if (!useExisting && !csv.trim()) {
      setError("请选择 CSV 文件");
      return;
    }
    setBusy(true);
    setError("");
    const token = ++requestVersion.current;
    try {
      const result = await api<BillPreview>(`/cycles/${effectiveCycle.id}/bill-previews`, "POST", {
        version: effectiveCycle.version,
        end,
        allocation_mode: selectedMode,
        ...(useExisting ? { import_id: effectiveCycle.bill_import_id } : { csv, file_name: fileName || "usage.csv" }),
      });
      if (token !== requestVersion.current) return;
      setPreview(result);
      setAcceptDifference(false);
      setAcceptOutside(false);
    } catch (e) {
      if (token !== requestVersion.current) return;
      setError((e as Error).message);
      setPreview(null);
    } finally {
      if (token === requestVersion.current) setBusy(false);
    }
  }
  async function confirm() {
    if (!preview || !preview.can_confirm) return;
    if (preview.difference_required && !acceptDifference) {
      setError("请确认未匹配记录的差异");
      return;
    }
    if (preview.outside_count > 0 && !acceptOutside) {
      setError("请确认周期范围外的账单记录");
      return;
    }
    setBusy(true);
    setError("");
    try {
      await api(`/cycles/${effectiveCycle.id}/bill-confirm`, "POST", {
        version: effectiveCycle.version,
        preview_id: preview.id,
        accept_difference: acceptDifference,
        accept_outside: acceptOutside,
      });
      setPreview(null);
      await onConfirmed();
    } catch (e) {
      setError((e as Error).message);
    } finally {
      setBusy(false);
    }
  }
  return (
    <div className="bill-import">
      <div className="bill-import-head"><strong>上游账单</strong><span>{effectiveCycle.cost_source === "upstream_bill" ? "替换账单" : "模糊成本"}</span></div>
      <label>分摊方式<Select value={mode} disabled={busy} onChange={(e) => {
        const next = e.target.value as BillMode;
        setMode(next); ++requestVersion.current; setPreview(null); setAcceptDifference(false); setAcceptOutside(false);
        if (csv.trim() || effectiveCycle.bill_import_id) void inspect(!csv.trim(), next);
      }}><option value="proportional">比例分摊</option><option value="matched_only">精准匹配</option></Select></label>
      <label>CSV 文件
        <input type="file" accept=".csv,text/csv" onChange={async (e) => {
          const file = e.target.files?.[0];
          if (!file) return;
          if (file.size > 10 * 1024 * 1024) { setError("CSV 不能超过 10 MB"); return; }
          const token = ++fileRead.current;
          ++requestVersion.current;
          setBusy(false);
          setPreview(null);
          const content = await file.text();
          if (token !== fileRead.current) return;
          setFileName(file.name);
          setCsv(content);
        }} />
      </label>
      <div className="start-actions">
        <button className="button" disabled={busy || !csv.trim() || !end} onClick={() => inspect()}>{busy && !preview ? "匹配中…" : "账单预览"}</button>
        {!!effectiveCycle.bill_import_id && <button className="button" disabled={busy} onClick={() => inspect(true)}>沿用已导入账单</button>}
      </div>
      {preview && <div className="bill-stats">
        <div><span>文件</span><b>{preview.file_name}</b></div>
        <div><span>上游总扣费</span><b>{money(preview.cost_fen)}</b></div>
        <div><span>本次计费</span><b>{money(preview.billable_cost_fen ?? preview.cost_fen)}</b></div>
        <div><span>已匹配</span><b>{preview.matched_count} 条 · {billedMoney(preview.matched_amount)}</b></div>
        <div><span>{preview.allocation_mode === "matched_only" ? "未匹配不计费" : "未匹配"}</span><b>{preview.unmatched_count} 条 · {billedMoney(preview.unmatched_amount)}</b></div>
        {preview.coverage_warning && <FormError error={preview.coverage_warning} />}
        {!!preview.local_unmatched_count && <div><span>本地未列入账单</span><b>{preview.local_unmatched_count} 条</b></div>}
        {!!preview.outside_count && <div><span>范围外</span><b>{preview.outside_count} 条 · {billedMoney(preview.outside_amount)}</b></div>}
        {(preview.unmatched_count > 0 || preview.outside_count > 0) && <button className="text-button" disabled={busy} onClick={async () => {
          const token = requestVersion.current;
          setBusy(true); setError("");
          try {
            await downloadCsv(`/cycles/${effectiveCycle.id}/bill-previews/${preview.id}/unmatched.csv`, "未匹配账单.csv");
          } catch (e) {
            if (token === requestVersion.current) setError((e as Error).message);
          } finally {
            if (token === requestVersion.current) setBusy(false);
          }
        }}>导出未匹配明细</button>}
        {preview.difference_required && <label className="checkline"><input type="checkbox" checked={acceptDifference} onChange={(e) => setAcceptDifference(e.target.checked)} />确认本地差异，按已匹配扣费比例分摊</label>}
        {!!preview.outside_count && <label className="checkline"><input type="checkbox" checked={acceptOutside} onChange={(e) => setAcceptOutside(e.target.checked)} />确认不计入范围外记录</label>}
        <BillAllocation cycle={effectiveCycle} preview={preview} />
        {!preview.can_confirm && <FormError error="没有正金额匹配依据，无法分摊" />}
        <button className="button primary" disabled={busy || !preview.can_confirm || (preview.difference_required && !acceptDifference) || (!!preview.outside_count && !acceptOutside)} onClick={confirm}>{busy ? "确认中…" : preview.allocation_mode === "matched_only" ? "确认仅按匹配金额计费" : "确认替换成本"}</button>
      </div>}
      {error && <div className="form-error">{error}</div>}
    </div>
  );
}

export function SettleForm({
  ids,
  expenseIds = [],
  expenseRevision = "",
  cycles = [],
  catalog,
  done,
  close,
  renewByDefault = false,
}: {
  ids: number[];
  expenseIds?: number[];
  expenseRevision?: string;
  cycles?: Cycle[];
  catalog: Catalog;
  done: (message?: string) => void;
  close: () => void;
  renewByDefault?: boolean;
}) {
  const [end, setEnd] = useState(() => inputDate());
  const [displayPreview, setDisplayPreview] = useState<Preview | null>(null);
  const [renew, setRenew] = useState<
    Record<number, { cost: string; creditor_id: number; cost_kind: CostKind }>
  >({});
  const renewalDefaultsSet = useRef(false);
  const [deleteIds, setDeleteIds] = useState<number[]>([]);
  const [deleteScopes,setDeleteScopes] = useState<Record<number,DeletionScopeValue|null>>({});
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");
  const { preview, loading, error: previewError, refresh: inspect } = useAutoPreview({
    ids, expenseIds, end, paused: busy,
    revision: JSON.stringify(cycles.map(c => [c.id, c.version])) + expenseRevision,
    onResult(next) {
      setDisplayPreview(next);
      if (renewByDefault && !renewalDefaultsSet.current) {
        setRenew(Object.fromEntries(next.cycles.map((c) => [c.cycle.id, { cost: costInput(c.cycle.cost_fen), creditor_id: c.cycle.creditor_id, cost_kind: c.cycle.cost_source === "upstream_bill" ? "estimated" : c.cycle.cost_kind || "exact" }])));
        renewalDefaultsSet.current = true;
      }
    },
  });
  const pendingSubmission = useRef<{preview_id: string; renewals: {cycle_id: number; cost: string; creditor_id: number; cost_kind: CostKind}[]; delete_cycle_ids?: number[];delete_scopes?:Record<string,string>} | null>(null);
  let cutoff = "";
  try { if (end) cutoff = fromInput(end); } catch { /* Date input may be incomplete. */ }
  const cycleMap = new Map<number, Cycle>();
  for (const cycle of cycles) cycleMap.set(cycle.id, cycle);
  for (const calculation of displayPreview?.cycles || []) {
    const current = cycleMap.get(calculation.cycle.id);
    if (!current || current.version <= calculation.cycle.version) cycleMap.set(calculation.cycle.id, calculation.cycle);
  }
  for (const [id, cycle] of cycleMap) {
    const name = catalog.accounts.find(a => a.id === cycle.account_id)?.name;
    if (name) cycleMap.set(id, { ...cycle, name });
  }
  const billCycles = [...cycleMap.values()].filter(canImportBill);
  const calculations = new Map((preview?.cycles || []).map((calculation) => [calculation.cycle.id, calculation]));
  async function settle() {
    if (!pendingSubmission.current && deleteIds.some(id=>!deleteScopes[id])) {setError("请先核验并确认删除范围");return;}
    if ((!preview || loading) && !pendingSubmission.current) return;
    if (!pendingSubmission.current && preview && preview.cycles.some((c) => c.can_settle === false)) {
      setError("请先确认起点、成本或账单");
      return;
    }
    if (!pendingSubmission.current && preview && Date.parse(preview.expires_at) <= Date.now()) {
      await inspect();
      return;
    }
    if (!pendingSubmission.current && Object.values(renew).some((x) => !amountPattern.test(x.cost))) {
      setError("续期成本最多保留两位小数");
      return;
    }
    setBusy(true);
    setError("");
    try {
      const payload = pendingSubmission.current || {
        preview_id: preview!.id,
        renewals: Object.entries(renew).map(([id, r]) => ({
          cycle_id: Number(id),
          ...r,
        })),
        ...(deleteIds.length ? { delete_cycle_ids: deleteIds,delete_scopes:Object.fromEntries(deleteIds.map(id=>[id,deleteScopes[id]!.digest])) } : {}),
      };
      pendingSubmission.current = payload;
      const result = await api<{ deletions?: DeletionJob[] }>("/settlements", "POST", payload);
      pendingSubmission.current = null;
      done(result.deletions?.some(j => ["failed", "verify", "cancelled"].includes(j.status)) ? "已出单，删除未完成。请在管理视图中查看。" : result.deletions?.some(j => j.status !== "done") ? "已出单，正在删除账号" : result.deletions?.length ? "已出单并删除账号" : undefined);
    } catch (e) {
      setError((e as Error).message);
      if (e instanceof ApiError && e.status < 500) pendingSubmission.current = null;
    } finally {
      setBusy(false);
    }
  }
  return (
    <Modal
      title={`结算预览 · ${ids.length + expenseIds.length} 项`}
      wide
      close={() => !busy && close()}
    >
      <div className="form">
        <FormError error={error} />
        <FormError error={previewError} />
        {previewError && <button className="button" disabled={busy} onClick={() => inspect()}>重试</button>}
        {loading && <p className="preview-progress" role="status">正在核算分摊…</p>}
        <div className="end-picker">
          {!!ids.length && <label>
            结束时间（北京时间）
            <input
              type="datetime-local"
              step="1"
              value={end}
              disabled={busy || !!pendingSubmission.current}
              onChange={(e) => setEnd(e.target.value)}
            />
          </label>}
        </div>
        <fieldset className="settlement-fields" disabled={busy || !!pendingSubmission.current}>
        {!!billCycles.length && (
          <div className="bill-import-list">
            {billCycles.map((cycle) => (
              <BillImportPanel
                key={cycle.id}
                cycle={cycle}
                calculation={calculations.get(cycle.id)}
                end={cutoff}
                previewKey={preview?.id || ""}
                onConfirmed={() => inspect()}
              />
            ))}
          </div>
        )}
        {displayPreview?.cycles.map((c) => (
          <div key={c.cycle.id}>
            <Allocation value={{ ...c, cycle: { ...c.cycle, name: cycleMap.get(c.cycle.id)?.name || c.cycle.name } }} />
            {!c.start_confirmed && (c.start_mode === "date_estimate" || c.start_mode === "minute_estimate") && (
              <StartConfirmation calculation={c} onConfirmed={() => inspect()} />
            )}
            {c.start_warning && c.start_warning !== "周期起点未确认" && <div className="form-error">{c.start_warning}</div>}
            {c.cost_warning && <FormError error={c.cost_warning} />}
            <label className="checkline renewal">
              <input
                type="checkbox"
                checked={!!renew[c.cycle.id]}
                disabled={deleteIds.includes(c.cycle.id)}
                onChange={(e) =>
                  setRenew((prev) => {
                    const next = { ...prev };
                    if (e.target.checked)
                      next[c.cycle.id] = {
                        cost: costInput(c.cycle.cost_fen),
                        creditor_id: c.cycle.creditor_id,
                        cost_kind: c.cycle.cost_source === "upstream_bill" ? "estimated" : c.cycle.cost_kind || "exact",
                      };
                    else delete next[c.cycle.id];
                    return next;
                  })
                }
              />
              结算后续期
            </label>
            <label className="checkline deletion-option"><input type="checkbox" checked={deleteIds.includes(c.cycle.id)} disabled={!!renew[c.cycle.id]} onChange={e => setDeleteIds(prev => e.target.checked ? [...prev, c.cycle.id] : prev.filter(id => id !== c.cycle.id))} />出单后删除账号</label>
            {deleteIds.includes(c.cycle.id) && <DeletionScope accountId={c.cycle.account_id} onConfirmed={value=>setDeleteScopes(prev=>({...prev,[c.cycle.id]:value}))}/>}
            {renew[c.cycle.id] && (
              <div className="form-columns">
                <label>新周期成本类型<Select value={renew[c.cycle.id].cost_kind} onChange={(e) => setRenew({...renew, [c.cycle.id]: {...renew[c.cycle.id], cost_kind: e.target.value as CostKind}})}><option value="exact">准确成本</option><option value="estimated">模糊成本</option></Select></label>
                <label>
                  新周期成本（元）
                  <input
                    inputMode="decimal"
                    value={renew[c.cycle.id].cost}
                    onChange={(e) =>
                      setRenew({
                        ...renew,
                        [c.cycle.id]: {
                          ...renew[c.cycle.id],
                          cost: e.target.value,
                        },
                      })
                    }
                  />
                </label>
                <label>
                  垫付人
                  <Select
                    value={renew[c.cycle.id].creditor_id}
                    onChange={(e) =>
                      setRenew({
                        ...renew,
                        [c.cycle.id]: {
                          ...renew[c.cycle.id],
                          creditor_id: Number(e.target.value),
                        },
                      })
                    }
                  >
                    {catalog.users.map((u) => (
                      <option key={u.id} value={u.id}>
                        {u.name}
                      </option>
                    ))}
                  </Select>
                </label>
              </div>
            )}
          </div>
        ))}
        {displayPreview?.expenses?.map((expense) => (
          <div key={expense.expense.id}>
            <Allocation value={expense} />
          </div>
        ))}
        </fieldset>
        <footer>
          <button className="button" disabled={busy} onClick={close}>
            取消
          </button>
          <button
            className="button primary"
            disabled={busy || (!pendingSubmission.current && (loading || !preview || preview.cycles.some((c) => c.can_settle === false) || deleteIds.some(id=>!deleteScopes[id])))}
            onClick={settle}
          >
            {busy ? "结算中…" : pendingSubmission.current ? "重试结算" : "确认结算"}
          </button>
        </footer>
      </div>
    </Modal>
  );
}

export function App() {
  const [me, setMe] = useState<Actor | null>(null);
  const [overviewSource, setOverview] = useState<Overview | null>(null);
  const [catalogSource, setCatalog] = useState<Catalog | null>(null);
  const [tab, setTab] = useState("accounts");
  const [costView, setCostView] = useState<"accounts" | "expenses">("accounts");
  const [manageTab, setManageTab] = useState("cycles");
  const [query, setQuery] = useState("");
  const [filter, setFilter] = useState("all");
  const [orders, setOrders] = useState<Order[]>([]);
  const [balances, setBalances] = useState<NetBalance[]>([]);
  const [payments, setPayments] = useState<NetPayment[]>([]);
  const [expenses, setExpenses] = useState<ExpenseCalculation[]>([]);
  const [historySource, setHistory] = useState<{ cycles: Cycle[]; audit: any[]; account_revision?: string }>({
    cycles: [],
    audit: [],
  });
  const [inbox, setInbox] = useState<{ items: any[]; unread: number }>({
    items: [],
    unread: 0,
  });
  const [selected, setSelected] = useState<number[]>([]);
  const [form, setForm] = useState<{ account?: Account; cycle?: Cycle; expense?: ExpenseCalculation; newExpense?: boolean; upgrade?: Account } | null>(
    null,
  );
  const [settleIds, setSettleIds] = useState<number[] | null>(null);
  const [settleExpenseIds, setSettleExpenseIds] = useState<number[] | null>(null);
  const [settleRenewDefault, setSettleRenewDefault] = useState(false);
  const [detail, setDetail] = useState<any>(null);
  const [showInbox, setShowInbox] = useState(false);
  const [startCycle, setStartCycle] = useState<Cycle | null>(null);
  const [confirm, setConfirm] = useState<{
    text: string;
    action: (memo?: {note?:string;reference?:string}) => Promise<unknown>;
    payment?: boolean;
    deletionScope?: DeletionScopeValue;
  } | null>(null);
  const [paymentNote, setPaymentNote] = useState("");
  const [paymentReference, setPaymentReference] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");
  const [toast, setToast] = useState("");
  const isAdmin = me?.role === "admin";
  const [deletions, setDeletions] = useState<DeletionJob[]>([]);
  const { state: accountState, refresh: refreshAccountState } = useAccountState(me?.id);
  const active = accountState?.revision ? new Set(accountState.active_ids) : null;
  const latestNames = new Map((accountState?.accounts || []).map(a => [a.id, a.name]));
  const currentName = (id: number, fallback: string, revision = "") =>
    accountState && accountState.revision >= revision ? latestNames.get(id) ?? fallback : fallback;
  const currentCycle = (c: Cycle, revision = "") => c.status === "open" ? { ...c, name: currentName(c.account_id, c.name, revision) } : c;
  const overview = overviewSource && { ...overviewSource, accounts: overviewSource.accounts.filter(a => !active || active.has(a.id)).map(a => ({ ...a, name: currentName(a.id, a.name, overviewSource.account_revision), cycle: a.cycle ? currentCycle(a.cycle, overviewSource.account_revision) : null })) };
  const catalog = catalogSource && { ...catalogSource, accounts: catalogSource.accounts.filter(a => !active || active.has(a.id)).map(a => ({ ...a, name: currentName(a.id, a.name, catalogSource.account_revision) })) };
  const history = { ...historySource, cycles: historySource.cycles.filter(c => !active || active.has(c.account_id)).map(c => currentCycle(c, historySource.account_revision)) };
  const refreshSequence = useRef(0);

  useEffect(() => {
    if (!active) return;
    const known = [...(overviewSource?.accounts.flatMap(a => a.cycle ? [a.cycle] : []) || []), ...historySource.cycles];
    const removed = new Set(known.filter(c => !active.has(c.account_id)).map(c => c.id));
    if ((tab === "accounts" && costView === "accounts") || (tab === "manage" && manageTab === "cycles")) {
      setSelected(prev => prev.some(id => removed.has(id)) ? prev.filter(id => !removed.has(id)) : prev);
    }
    if (settleIds?.some(id => removed.has(id))) { setSettleIds(null); setError("账号已删除，结算窗口已关闭"); }
    const editedAccount = form?.account?.id ?? form?.upgrade?.id ?? form?.cycle?.account_id;
    if (editedAccount && !active.has(editedAccount)) { setForm(null); setError("账号已删除，请选择其他账号"); }
    if (startCycle && !active.has(startCycle.account_id)) setStartCycle(null);
  }, [accountState, overviewSource, historySource, tab, costView, manageTab]);

  useEffect(() => {
    document.documentElement.dataset.theme =
      new URLSearchParams(location.search).get("theme") === "dark"
        ? "dark"
        : "light";
    api<Actor>("/me")
      .then((u) => {
        setCsrf(u.csrf, u.id);
        setMe(u);
      })
      .catch((e) => setError(e.message));
  }, []);
  useEffect(() => {
    const clear = () => {
      setMe(null);
      setOverview(null);
      setCatalog(null);
      setHistory({cycles: [], audit: []});
      setDeletions([]);
      ++refreshSequence.current;
      setOrders([]);
      setBalances([]);
      setPayments([]);
      setExpenses([]);
      setInbox({ items: [], unread: 0 });
      setForm(null);
      setSettleIds(null);
      setSettleExpenseIds(null);
      setDetail(null);
      setConfirm(null);
      setShowInbox(false);
      setStartCycle(null);
    };
    window.addEventListener("settlement:expired", clear);
    return () => window.removeEventListener("settlement:expired", clear);
  }, []);
  async function refresh() {
    if (!me) return;
    const sequence = ++refreshSequence.current;
    try {
      const [data, messages, accounts] = await Promise.all([
        api<Overview>("/overview"),
        api("/inbox"),
        isAdmin ? api<Catalog>("/catalog") : Promise.resolve(null),
      ]);
      if (sequence !== refreshSequence.current) return;
      setOverview(data);
      setInbox(messages);
      setCatalog(accounts);
      if (tab === "receivable") {
        setBalances((await api<{ balances: NetBalance[] }>("/netting/balances?scope=receivable")).balances);
      } else if (tab === "bills" || (tab === "manage" && manageTab === "orders")) {
        const scope = tab === "bills" ? "mine" : "all";
        const [raw, net, paid] = await Promise.all([
          api<{orders: Order[]}>("/orders?scope=" + scope),
          api<{balances: NetBalance[]}>("/netting/balances?scope=" + scope),
          api<{payments: NetPayment[]}>("/netting/payments?scope=" + scope),
        ]);
        if (sequence !== refreshSequence.current) return;
        setOrders(raw.orders); setBalances(net.balances); setPayments(paid.payments);
      }
      if (tab === "accounts") {
        const result = await api<{ expenses: ExpenseCalculation[] }>("/expenses");
        if (sequence !== refreshSequence.current) return;
        setExpenses(result.expenses);
      }
      if (isAdmin && tab === "manage") {
        const [past, jobs] = await Promise.all([api("/history"), api<{items: DeletionJob[]}>("/account-deletions")]);
        if (sequence !== refreshSequence.current) return;
        setHistory(past); setDeletions(jobs.items);
      }
    } catch (e) {
      setError((e as Error).message);
    }
  }
  useEffect(() => {
    if (!me) return;
    void refresh();
    const timer = setInterval(() => {
      if (!document.hidden) void refresh();
    }, 10_000);
    const visible = () => {
      if (!document.hidden) void refresh();
    };
    document.addEventListener("visibilitychange", visible);
    return () => {
      ++refreshSequence.current;
      clearInterval(timer);
      document.removeEventListener("visibilitychange", visible);
    };
  }, [me, tab, manageTab, costView]);
  const discoveredAccounts = useRef("");
  useEffect(() => {
    if (!accountState?.accounts || !overviewSource || accountState.error) return;
    const known = new Set(overviewSource.accounts.map(a => a.id));
    const added = accountState.active_ids.filter(id => !known.has(id)).sort((a, b) => a - b).join(",");
    if (added && added !== discoveredAccounts.current) void refresh();
    discoveredAccounts.current = added;
  }, [accountState, overviewSource]);
  useEffect(() => {
    if (!toast) return;
    const timer = setTimeout(() => setToast(""), 3500);
    return () => clearTimeout(timer);
  }, [toast]);
  function move(next: string) {
    setTab(next);
    setSelected([]);
    setQuery("");
    setFilter("all");
    setError("");
  }
  function toggle(id: number) {
    setSelected((s) =>
      s.includes(id) ? s.filter((x) => x !== id) : [...s, id],
    );
  }
  async function run(action: () => Promise<unknown>, message = "已保存") {
    setBusy(true);
    setError("");
    try {
      await action();
      setConfirm(null);
      setSelected([]);
      setToast(message);
      await refresh();
    } catch (e) {
      setError((e as Error).message);
      if (e instanceof ApiError && e.status === 409) {
        setConfirm(null); setDetail(null); await refresh();
      }
    } finally {
      setBusy(false);
    }
  }
  async function saved(message?: string) {
    setError("");
    setForm(null);
    setSettleIds(null);
    setSettleExpenseIds(null);
    setSelected([]);
    setToast(message || "已保存");
    await refreshAccountState();
    await refresh();
  }
  async function showOrder(id: number) {
    try {
      setDetail(await api(`/orders/${id}`));
    } catch (e) {
      setError((e as Error).message);
    }
  }
  async function showCycle(id: number) {
    try {
      setDetail({ snapshot: await api(`/cycles/${id}`) });
    } catch (e) {
      setError((e as Error).message);
    }
  }
  function orderAction(o: Order) {
    const group = orders.filter(row => o.batch_id ? row.batch_id === o.batch_id : row.id === o.id);
    setConfirm({text: `标记 ${group.length} 张历史待处理订单已结清 ${money(group.reduce((sum, row) => sum + row.amount_fen, 0))}`, action: () => api(`/orders/${o.id}/actions`, "POST", {action: "mark_paid", version: o.version})});
  }
  async function showNetting(value: NetBalance | NetPayment) {
    try {
      const path = "id" in value ? `/netting/payments/${value.id}` : `/netting/balances/${value.user_a}/${value.user_b}`;
      setDetail({netting: await api(path)});
    } catch (e) { setError((e as Error).message); }
  }
  function payNetting(value: NetBalance) {
    const payerName = value.payer_id === value.user_a ? value.name_a : value.name_b;
    const creditorName = value.creditor_id === value.user_a ? value.name_a : value.name_b;
    const text = value.amount_fen ? `${payerName} 向 ${creditorName} 转账 ${money(value.amount_fen)}，结清双方 ${value.order_count} 张订单` : `确认 ${value.name_a} 与 ${value.name_b} 的 ${value.order_count} 张订单相互抵消，无需转账`;
    const payload = {user_a: value.user_a, user_b: value.user_b, digest: value.digest};
    setPaymentNote(""); setPaymentReference("");
    setConfirm({text, payment:true, action: memo => api("/netting/payments", "POST", {...payload,...memo})});
  }
  async function retryDeletion(job:DeletionJob) {
    setBusy(true);setError("");
    try{const scope=await api<DeletionScopeValue>(`/accounts/${job.account_id}/deletion-scope`);setConfirm({text:`重试删除以下 ${scope.targets.length} 个账号`,deletionScope:scope,action:()=>api(`/account-deletions/${job.id}/retry`,"POST",{version:job.version,scope_digest:scope.digest})});}
    catch(e){setError((e as Error).message);}finally{setBusy(false);}
  }
  const filteredAccounts = (overview?.accounts || []).filter(
    (a) =>
      isOperationalAccount(a) &&
      (isAdmin || a.kind === "paid") &&
      (!query ||
        `${a.name} ${a.id}`.toLowerCase().includes(query.toLowerCase())) &&
      (filter === "all" || filter === "open"
        ? filter === "all" || (a.kind === "paid" && a.cycle?.status === "open")
        : a.kind === filter),
  );
  const filteredExpenses = expenses.filter((item) =>
    !query || `${item.expense.name} ${item.expense.id} ${item.creditor_name || ""}`.toLowerCase().includes(query.toLowerCase()),
  );
  const billPage = tab === "bills" || (tab === "manage" && manageTab === "orders");
  const receivablePage = tab === "receivable";
  const availableCycles = new Map<number, Cycle>();
  for (const account of overview?.accounts || []) {
    if (account.cycle) availableCycles.set(account.cycle.id, account.cycle);
  }
  for (const cycle of history.cycles) availableCycles.set(cycle.id, cycle);
  const settleCycles = (settleIds || [])
    .map((id) => availableCycles.get(id))
    .filter((cycle): cycle is Cycle => !!cycle);

  return (
    <div className="app">
      <header className="topbar">
        <div className="brand">
          <div className="brand-icon" aria-hidden="true" dangerouslySetInnerHTML={{ __html: settlementIcon }} />
          <h1>结算中心</h1>
        </div>
        <div className="header-right">
          {me && (
            <span className="identity">
              {me.name}
              <small>{isAdmin ? "管理员" : "成员"}</small>
            </span>
          )}
          <button
            className="icon-button bell"
            aria-label="通知"
            onClick={() => {
              setShowInbox(true);
              if (inbox.unread) void run(() => api("/inbox/read", "POST"), "");
            }}
          >
            <Bell size={19} />
            {inbox.unread > 0 && <span>{Math.min(inbox.unread, 99)}</span>}
          </button>
        </div>
      </header>
      <nav className="tabs" aria-label="结算导航">
        {[
          { key: "accounts", title: "账号成本", icon: Layers3 },
          { key: "bills", title: "我的账单", icon: Wallet },
          { key: "receivable", title: "收款", icon: ArrowDownLeft },
          ...(isAdmin
            ? [{ key: "manage", title: "管理", icon: Settings2 }]
            : []),
        ].map((t) => (
          <button
            key={t.key}
            className={tab === t.key ? "active" : ""}
            onClick={() => move(t.key)}
          >
            <t.icon size={16} />
            {t.title}
          </button>
        ))}
      </nav>
      <main>
        <FormError error={error} />
        {overview?.sync_error && <FormError error={overview.sync_error} />}
        {(accountState?.error || overview?.account_sync_error) && <FormError error={accountState?.error || overview?.account_sync_error || ""} />}
        {!me ? (
          <Empty text={error ? "从 Sub2API 菜单重新进入" : "正在验证登录…"} />
        ) : !overview ? (
          <Empty text="正在读取账目…" />
        ) : (
          <>
            {tab !== "manage" && !receivablePage && (
              <section className="balance-strip">
                <div className="balance-main">
                  <span>待付款</span>
                  <strong>{money(overview.pending_fen)}</strong>
                </div>
                <div>
                  <span>预计待付</span>
                  <strong>
                    {overview.estimated_fen === null
                      ? "—"
                      : money(overview.estimated_fen)}
                  </strong>
                </div>
                <div>
                  <span>待收款</span>
                  <strong>{money(overview.receivable_fen)}</strong>
                </div>
              </section>
            )}
            {tab === "accounts" && (
              <div className="segments cost-tabs" role="tablist" aria-label="成本类型">
                <button className={costView === "accounts" ? "active" : ""} onClick={() => { setCostView("accounts"); setQuery(""); setSelected([]); }} role="tab" aria-selected={costView === "accounts"}>账号费用</button>
                <button className={costView === "expenses" ? "active" : ""} onClick={() => { setCostView("expenses"); setQuery(""); setSelected([]); }} role="tab" aria-selected={costView === "expenses"}>固定费用</button>
              </div>
            )}
            {tab === "manage" && <OperationStatus />}
            {tab === "manage" && (
              <div className="segments manage-tabs">
                {[
                  ["cycles", "周期"],
                  ["orders", "全部订单"],
                  ["audit", "操作记录"],
                  ["deletions", "账号删除"],
                ].map(([key, text]) => (
                  <button
                    key={key}
                    className={manageTab === key ? "active" : ""}
                    onClick={() => {
                      setManageTab(key);
                      setSelected([]);
                      setFilter("all");
                      setQuery("");
                    }}
                  >
                    {text}
                  </button>
                ))}
              </div>
            )}
            {((tab === "accounts") || billPage) && (
              <div className="toolbar">
                <div className="search">
                  <Search size={16} />
                  <input
                    placeholder={
                      tab === "accounts" ? (costView === "expenses" ? "搜索固定费用" : "搜索账号") : "搜索订单、账号或用户"
                    }
                    aria-label="搜索"
                    value={query}
                    onChange={(e) => setQuery(e.target.value)}
                  />
                </div>
                {(tab !== "accounts" || costView === "accounts") && <Select
                    className="filter"
                    aria-label="状态筛选"
                    value={filter}
                    onChange={(e) => {
                      setFilter(e.target.value);
                      setSelected([]);
                    }}
                  >
                    {(tab === "accounts"
                      ? [
                          ["all", "全部账号"],
                          ["open", "进行中"],
                          ...(isAdmin ? [["unconfigured", "待配置"], ["free", "免费资源"]] : []),
                        ]
                      : [
                          ["all", "全部状态"],
                          ["pending", "待付款"],
                          ["submitted", "待管理员处理"],
                          ["paid", "已结清"],
                          ["void", "已作废"],
                        ]
                    ).map(([v, label]) => (
                      <option value={v} key={v}>{label}</option>
                    ))}
                  </Select>}
                <div className="toolbar-actions">
                  {tab === "accounts" && costView === "accounts" && isAdmin && selected.length > 0 && (
                      <button
                        className="button primary"
                        disabled={!catalog}
                        onClick={() => { setSettleRenewDefault(false); setSettleIds(selected); }}
                      >
                        批量结算 ({selected.length})
                      </button>
                  )}
                  {tab === "accounts" && costView === "expenses" && isAdmin && (
                    <button className="button primary" disabled={!catalog} onClick={() => setForm({newExpense: true})}>
                      <Plus size={16} />新增固定费用
                    </button>
                  )}
                </div>
              </div>
            )}
            {tab === "accounts" && costView === "accounts" && (
              <div className="table-shell">
                <table className="accounts-table">
                  <thead>
                    <tr>
                      {isAdmin && (
                        <th className="check-cell">
                          <span className="sr-only">选择</span>
                        </th>
                      )}
                      <th>账号 / 周期</th>
                      <th className="numeric">成本</th>
                      <th>垫付人</th>
                      <th className="numeric">我的占比</th>
                      <th className="numeric">预计分摊</th>
                      <th>状态</th>
                      <th className="action-cell">操作</th>
                    </tr>
                  </thead>
                  <tbody>
                    {filteredAccounts.map((a) => (
                      <tr key={a.id} className={a.kind !== "paid" ? "compact-account" : undefined}>
                        {isAdmin && (
                          <td className="check-cell">
                            {a.kind === "paid" && a.cycle?.status === "open" && <input
                              aria-label={`选择 ${a.name}`}
                              type="checkbox"
                              checked={
                                !!a.cycle && selected.includes(a.cycle.id)
                              }
                              onChange={() => a.cycle && toggle(a.cycle.id)}
                            />}
                          </td>
                        )}
                        <td data-label="账号" className="account-name">
                          <div className="account-title">
                            <span className={"platform " + a.platform}>
                              {a.platform.slice(0, 1).toUpperCase()}
                            </span>
                            <div>
                              <strong>{a.name}</strong>
                              <small>
                                {a.kind === "free"
                                  ? "免费资源"
                                  : a.cycle
                                    ? `${!a.start_confirmed && a.start_mode === "date_estimate" ? "约 " + date(a.start_lower) + " — " + date(a.start_upper) : !a.start_confirmed && a.start_mode === "minute_estimate" ? "约 " + date(a.start_lower, "minute") : date(a.cycle.start)}${a.cycle.end ? " — " + date(a.cycle.end) : " 起"}`
                                    : a.platform}
                              </small>
                            </div>
                          </div>
                        </td>
                        <td data-label="成本" className="numeric amount">
                          {a.kind === "free" ? money(0) : a.cycle ? <>
                            <span className="cost-kind">{a.cycle.cost_kind === "estimated" ? "模糊成本" : "准确成本"}</span>
                            {money(a.cycle.cost_fen)}
                          </> : "—"}
                        </td>
                        <td data-label="垫付人">
                          {a.kind === "free" ? "—" : a.creditor_name || "—"}
                        </td>
                        <td data-label="我的占比" className="numeric">
                          {a.kind === "free" || !a.cycle || a.data_error ? (
                            "—"
                          ) : (
                            <div className="share-meter">
                              <span>
                                {(Number(a.mine?.share || 0) * 100).toFixed(2)}%
                              </span>
                              <div>
                                <i
                                  style={{
                                    width: `${Math.min(100, Number(a.mine?.share || 0) * 100)}%`,
                                  }}
                                />
                              </div>
                            </div>
                          )}
                        </td>
                        <td data-label="预计分摊" className="numeric">
                          <strong className="amount">
                            {a.kind === "free"
                              ? money(0)
                              : !a.cycle || a.data_error ||
                                  (overview.sync_error &&
                                    a.cycle.status === "open")
                                ? "—"
                                : money(a.mine?.amount_fen || 0)}
                          </strong>
                          {a.mine?.self_covered && a.kind !== "free" && (
                            <small>自行承担</small>
                          )}
                        </td>
                        <td data-label="状态">
                          {a.kind === "unconfigured" ? (
                            <span className="status neutral">待配置</span>
                          ) : a.kind === "free" ? (
                            <span className="status neutral">免费</span>
                          ) : a.data_error ? (
                            <span className="status warning">
                              <CircleAlert size={12} />
                              记录不完整
                            </span>
                          ) : a.start_warning ? (
                            <span className="status warning">
                              <CircleAlert size={12} />
                              起点未确认
                            </span>
                          ) : a.cost_warning ? (
                            <span className="status warning"><CircleAlert size={12} />{a.cost_warning}</span>
                          ) : a.drift ? (
                            <span className="status warning">
                              <CircleAlert size={12} />
                              记录有差异
                            </span>
                          ) : (
                            <Status value={a.cycle?.status || "open"} />
                          )}
                        </td>
                        <td className="action-cell">
                          {isAdmin && (
                            <div className="row-actions">
                              {a.cycle?.status === "open" && !a.start_confirmed && <button className="text-button" onClick={() => setStartCycle(a.cycle)}>确认起点</button>}
                              {canUpgradePaid(a) ? (
                                <button className="text-button primary-text" onClick={() => setForm({ upgrade: a })}>升级付费</button>
                              ) : (
                                <button className="text-button" onClick={() => setForm({ account: a, ...(a.cycle?.status === "open" ? { cycle: a.cycle } : {}) })}>
                                  {a.cycle?.status === "open" ? <><Edit3 size={13} />修改成本</> : a.kind === "unconfigured" ? "配置" : "新周期"}
                                </button>
                              )}
                              {a.kind === "paid" && a.cycle?.status === "open" && (
                                <>
                                  <button className="text-button" onClick={() => { setSettleRenewDefault(false); setSettleIds([a.cycle!.id]); }}>结算</button>
                                  <button className="text-button primary-text" onClick={() => { setSettleRenewDefault(true); setSettleIds([a.cycle!.id]); }}>结算并续费</button>
                                </>
                              )}
                            </div>
                          )}
                          {a.cycle && (
                            <button
                              className="icon-button"
                              aria-label={`${a.name} 周期明细`}
                              onClick={() => showCycle(a.cycle!.id)}
                            >
                              <ChevronRight size={17} />
                            </button>
                          )}
                        </td>
                      </tr>
                    ))}
                  </tbody>
                </table>
                {!filteredAccounts.length && <Empty text="暂无账号成本" />}
              </div>
            )}
            {tab === "accounts" && costView === "expenses" && (
              <div className="expense-list">
                {filteredExpenses.map((item) => {
                  const expense = item.expense;
                  const mine = item.allocations.find((a) => a.user_id === me.id);
                  return (
                    <article className="expense-row" key={expense.id}>
                      <div className="expense-main">
                        <div className="expense-title"><strong>{expense.name}</strong><span>#{expense.id} · {expense.incurred_on}</span></div>
                        <div className="expense-amount"><strong>{money(expense.cost_fen)}</strong><Status value={expense.status || "open"} /></div>
                      </div>
                      <div className="expense-meta">
                        <span>垫付人 <b>{item.creditor_name || "—"}</b></span>
                        <span>{isAdmin ? `${item.allocations.length} 人参与` : "我的分摊"} <b>{mine ? `${(Number(mine.share) * 100).toFixed(2)}% · ${money(mine.amount_fen)}` : "—"}</b></span>
                      </div>
                      <div className="expense-actions">
                        <button className="text-button" onClick={() => setDetail({snapshot: item})}>明细</button>
                        {isAdmin && expense.status === "settled" && <button className="text-button" onClick={() => setConfirm({text: `作废「${expense.name}」本次结算？已报已付或结清的账目无法作废。`, action: () => api(`/settlements/${expense.settlement_id}/void`, "POST", {})})}>作废重算</button>}
                      {isAdmin && expense.status === "open" && <>
                        <button className="text-button" onClick={() => setForm({ expense: item })}><Edit3 size={13} />修改</button>
                        <button className="text-button primary-text" onClick={() => { setSettleIds(null); setSettleExpenseIds([expense.id]); }}>结算</button>
                      </>}
                      </div>
                    </article>
                  );
                })}
                {!filteredExpenses.length && <Empty text="暂无固定费用" />}
              </div>
            )}
            {receivablePage && <NettingReceivables balances={balances.filter(b => b.creditor_id === me.id && b.amount_fen > 0)} />}
            {billPage && <NettingList me={me} balances={balances} payments={payments} orders={orders} adminView={tab === "manage"} filter={filter} query={query} busy={busy} onPay={payNetting} onDetail={showNetting} onOrder={showOrder} onLegacyPaid={orderAction} />}
            {tab === "manage" && manageTab === "cycles" && (
              <div className="table-shell">
                <table className="history-table">
                  <thead>
                    <tr>
                      <th>账号 / 周期</th>
                      <th className="numeric">成本</th>
                      <th>垫付人</th>
                      <th>状态</th>
                      <th>操作</th>
                    </tr>
                  </thead>
                  <tbody>
                    {history.cycles.map((c) => (
                      <tr key={c.id}>
                        <td data-label="账号">
                          <strong>{c.name}</strong>
                          <small>
                            {date(c.start)} — {date(c.end)}
                          </small>
                        </td>
                        <td data-label="成本" className="numeric amount">
                          <span className="cost-kind">{c.cost_kind === "estimated" ? "模糊成本" : "准确成本"}</span>
                          {money(c.cost_fen)}
                        </td>
                        <td data-label="垫付人">{c.creditor_name}</td>
                        <td data-label="状态">
                          <Status value={c.status} />
                          {c.status === "open" && !c.start_confirmed && <small className="warning-text">起点未确认</small>}
                          {!!c.start_locked && <small className="locked-start">起点已锁定</small>}
                          {!!c.drift && (
                            <small className="warning-text">
                              消费记录有差异
                            </small>
                          )}
                        </td>
                        <td className="action-cell">
                          <button
                            className="text-button"
                            onClick={() => showCycle(c.id)}
                          >
                            明细
                          </button>
                          {c.status === "open" ? (
                            <>
                              <button
                                className="text-button"
                                onClick={() => setForm({ cycle: c })}
                              >
                                修改
                              </button>
                              <button
                                className="text-button"
                                onClick={() => setSettleIds([c.id])}
                              >
                                结算
                              </button>
                            </>
                          ) : (
                            <button
                              className="text-button danger"
                              onClick={() =>
                                setConfirm({
                                  text: `作废「${c.name}」本次结算，并恢复为待重算周期`,
                                  action: () =>
                                    api(
                                      `/settlements/${c.settlement_id}/void`,
                                      "POST",
                                    ),
                                })
                              }
                            >
                              作废
                            </button>
                          )}
                        </td>
                      </tr>
                    ))}
                  </tbody>
                </table>
                {!history.cycles.length && <Empty text="暂无周期" />}
              </div>
            )}
            {tab === "manage" && manageTab === "deletions" && <div className="deletion-list">
              {deletions.filter(j => !["done", "cancelled"].includes(j.status)).map(job => <div className="deletion-row" key={job.id}>
                <div><strong>{job.account_name}</strong><span>{({queued: "等待删除", running: "删除中", verify: "等待核验", failed: "删除未完成"} as Record<string, string>)[job.status] || job.status}</span>{job.error && <p className="form-error">{job.error}</p>}</div>
                <span className="muted">{date(job.updated_at)}</span>
                <div className="row-actions">{job.status === "failed" && <button className="text-button" disabled={busy} onClick={() => retryDeletion(job)}>重试删除</button>}{["failed", "queued"].includes(job.status) && <button className="text-button" disabled={busy} onClick={() => run(() => api(`/account-deletions/${job.id}/cancel`, "POST", {version: job.version}), "已取消删除任务")}>取消删除</button>}</div>
              </div>)}
              {!deletions.some(j => !["done", "cancelled"].includes(j.status)) && <Empty text="暂无待处理删除" />}
            </div>}
            {tab === "manage" && manageTab === "audit" && <AuditLog />}
          </>
        )}
      </main>
      {toast && (
        <div className="toast" role="status">
          <CircleCheck size={17} />
          {toast}
        </div>
      )}
      {(form?.expense || form?.newExpense) && catalog && (
        <ExpenseForm
          expense={form?.expense}
          catalog={catalog}
          done={saved}
          close={() => setForm(null)}
        />
      )}
      {form?.upgrade && catalog && (
        <UpgradeForm
          account={form.upgrade}
          catalog={catalog}
          done={() => { setFilter("all"); saved(); }}
          close={() => setForm(null)}
        />
      )}
      {form && !form.expense && !form.newExpense && !form.upgrade && catalog && (
        <CycleForm
          {...form}
          catalog={catalog}
          done={saved}
          close={() => setForm(null)}
        />
      )}
      {settleIds && catalog && (
        <SettleForm
          ids={settleIds}
          cycles={settleCycles}
          catalog={catalog}
          done={saved}
          close={() => { setSettleIds(null); setSettleRenewDefault(false); }}
          renewByDefault={settleRenewDefault}
        />
      )}
      {settleExpenseIds && catalog && (
        <SettleForm
          ids={[]}
          expenseIds={settleExpenseIds}
          expenseRevision={JSON.stringify(expenses.filter(e => settleExpenseIds.includes(e.expense.id)).map(e => [e.expense.id, e.expense.version]))}
          catalog={catalog}
          done={saved}
          close={() => setSettleExpenseIds(null)}
        />
      )}
      {startCycle && <Modal title="确认起点" close={() => setStartCycle(null)}><div className="form"><p className="candidate-result">当前估算：{date(startCycle.start)}</p><StartConfirmation calculation={{cycle: startCycle, end: "", total_consumption: "0", record_count: 0, allocations: [], start_lower: startCycle.start_lower}} onConfirmed={async () => { setStartCycle(null); await refresh(); setToast("起点已确认"); }} /></div></Modal>}
      {detail && (
        <Modal
          title={detail.netting ? "对冲明细" : detail.order ? `订单 #${detail.order.id}` : detail.snapshot.source_type === "fixed_expense" ? "固定费用明细" : "周期明细"}
          wide
          close={() => setDetail(null)}
        >
          <div className="form">
            {detail.netting ? <NettingDetails value={detail.netting} /> : <>
            {detail.drift && (
              <FormError error="结算后消费记录发生变化，原订单金额已保留" />
            )}
            {detail.order && (
              <div className="order-detail-top">
                <div>
                  <span>
                    {detail.order.payer_name} → {detail.order.creditor_name}
                  </span>
                  <strong>{money(detail.order.amount_fen)}</strong>
                </div>
                <Status value={detail.order.status} />
              </div>
            )}
            <Allocation value={detail.snapshot.display?.settled === false && detail.snapshot.cycle ? { ...detail.snapshot, cycle: currentCycle(detail.snapshot.cycle) } : detail.snapshot} />
            {!!detail.snapshot.cycle?.start_locked && (
              <p className="detail-time">付费起点已锁定 · {date(detail.snapshot.cycle.start)}</p>
            )}
            {detail.order?.submitted_at && (
              <p className="detail-time">
                付款时间 {date(detail.order.submitted_at)}
              </p>
            )}
            {detail.order?.paid_at && (
              <p className="detail-time">
                结清时间 {date(detail.order.paid_at)}
              </p>
            )}
            </>}
          </div>
        </Modal>
      )}
      {showInbox && (
        <Modal title="通知" close={() => setShowInbox(false)}>
          <div className="inbox">
            {inbox.items.map((n) => (
              <button
                key={n.id}
                onClick={() => {
                  if (n.order_id) {
                    setShowInbox(false);
                    void showOrder(n.order_id);
                  }
                }}
              >
                <div>
                  <strong>{n.title}</strong>
                  <time>{date(n.created_at)}</time>
                </div>
                {n.order_id && <ChevronRight size={16} />}
              </button>
            ))}
            {!inbox.items.length && <Empty text="暂无通知" />}
          </div>
        </Modal>
      )}
      {confirm && (
        <Modal title="确认操作" close={() => !busy && setConfirm(null)}>
          <div className="form">
            <p className="confirm-text">{confirm.text}</p>
            {confirm.deletionScope && <DeletionTargets value={confirm.deletionScope}/>}
            {confirm.payment && <>
              <label>备注（选填）<textarea maxLength={500} value={paymentNote} disabled={busy} onChange={e=>setPaymentNote(e.target.value)}/></label>
              <label>转账凭证编号（选填）<input maxLength={128} value={paymentReference} disabled={busy} onChange={e=>setPaymentReference(e.target.value)}/></label>
            </>}
            <FormError error={error} />
            <footer>
              <button
                className="button"
                disabled={busy}
                onClick={() => setConfirm(null)}
              >
                取消
              </button>
              <button
                className="button primary"
                disabled={busy}
                onClick={() => run(() => confirm.action(confirm.payment ? {...(paymentNote ? {note:paymentNote}:{}),...(paymentReference ? {reference:paymentReference}:{})} : undefined))}
              >
                {busy ? "处理中…" : "确认"}
              </button>
            </footer>
          </div>
        </Modal>
      )}
    </div>
  );
}

const rootElement = document.getElementById("root");
if (rootElement) createRoot(rootElement).render(<App />);
