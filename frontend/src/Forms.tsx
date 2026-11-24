import React, { useEffect, useRef, useState } from "react";
import {
  ArrowUpRight,
  CircleAlert,
  CircleCheck,
  Clock3,
  ReceiptText,
  X,
} from "lucide-react";
import {
  api,
  money,
  consumption,
  costInput,
  date,
  inputDate,
  fromInput,
  amountPattern,
  billedMoney,
} from "./api";
import type {
  Account,
  Catalog,
  Cycle,
  Calculation,
  ExpenseCalculation,
  StartMode,
  Share,
} from "./types";
import { Modal } from "./Modal";
import { Select } from "./Select";

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

export function Status({ value }: { value: string }) {
  const Icon = statusIcons[value] || Clock3;
  return (
    <span className={"status " + value}>
      <Icon size={12} />
      {statusName[value] || value}
    </span>
  );
}

export function Empty({ text }: { text: string }) {
  return (
    <div className="empty">
      <ReceiptText size={30} strokeWidth={1.3} />
      <span>{text}</span>
    </div>
  );
}

export function FormError({ error }: { error: string }) {
  return error ? (
    <div className="form-error" role="alert">
      <CircleAlert size={16} />
      {error}
    </div>
  ) : null;
}

export function CycleForm({
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
      catalog.accounts.find((a) => !a.deleted)?.id ||
      0,
  );
  const name =
    catalog.accounts.find((a) => a.id === aid)?.name ||
    account?.name ||
    cycle?.name ||
    "";
  const initialCost = cycle ? costInput(cycle.cost_fen) : "";
  const initialCreditor = cycle?.creditor_id || catalog.users[0]?.id || 0;
  const initialStart = inputDate(
    cycle?.start || account?.cycle?.end || new Date().toISOString(),
  );
  const initialStartMode: StartMode = cycle?.start_mode || "exact";
  const initialFree = !cycle && account?.kind === "free";
  const [cost, setCost] = useState(initialCost);
  const [creditor, setCreditor] = useState(initialCreditor);
  const [start, setStart] = useState(initialStart);
  const [startMode, setStartMode] = useState<StartMode>(initialStartMode);
  const [free, setFree] = useState(initialFree);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");
  const dirty =
    cost !== initialCost ||
    creditor !== initialCreditor ||
    start !== initialStart ||
    startMode !== initialStartMode ||
    free !== initialFree;

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
        cost_kind: "exact",
        creditor_id: creditor,
        start:
          startMode === "earliest_usage"
            ? null
            : cycle &&
                startMode === cycle.start_mode &&
                start === inputDate(cycle.start)
              ? cycle.start
              : fromInput(
                  startMode === "minute_estimate" ? start.slice(0, 16) : start,
                ),
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
      busy={busy}
      dirty={dirty}
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
            {cycle?.cost_source === "upstream_bill" &&
              cost !== costInput(cycle.cost_fen) && (
                <p className="field-hint">
                  修改金额将改用手动成本分摊，原账单保留。
                </p>
              )}
            <label>
              起点方式
              <Select
                value={startMode}
                onChange={(e) =>
                  setStartMode(e.target.value as typeof startMode)
                }
              >
                <option value="exact">精确时间</option>
                <option value="minute_estimate">按时间估算</option>
                {cycle?.start_mode === "date_estimate" && (
                  <option value="date_estimate">按日期估算</option>
                )}
                <option value="earliest_usage">从最早使用开始</option>
              </Select>
            </label>
            {startMode !== "earliest_usage" ? (
              <label>
                {startMode === "date_estimate"
                  ? "周期日期（北京时间）"
                  : "周期开始（北京时间）"}
                <input
                  type={startMode === "date_estimate" ? "date" : "datetime-local"}
                  step={startMode === "minute_estimate" ? 60 : 1}
                  required
                  value={
                    startMode === "date_estimate"
                      ? start.slice(0, 10)
                      : startMode === "minute_estimate"
                        ? start.slice(0, 16)
                        : start
                  }
                  onChange={(e) =>
                    setStart(
                      startMode === "date_estimate"
                        ? `${e.target.value}T00:00:00`
                        : e.target.value,
                    )
                  }
                />
              </label>
            ) : (
              <p className="field-hint">
                最早已采集使用：
                {date(
                  catalog.accounts.find((a) => a.id === aid)?.earliest_usage_at,
                )}
              </p>
            )}
            {startMode !== "earliest_usage" && (
              <p className="field-hint">
                可用记录起点：{date(catalog.coverage_from)}
              </p>
            )}
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

export function UpgradeForm({
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
  const name =
    catalog.accounts.find((a) => a.id === account.id)?.name || account.name;
  const initialCreditor = catalog.users[0]?.id || 0;
  const [cost, setCost] = useState("");
  const [creditor, setCreditor] = useState(initialCreditor);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");
  const dirty = cost !== "" || creditor !== initialCreditor;
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
        cost_kind: "exact",
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
    <Modal
      title="升级付费"
      busy={busy}
      dirty={dirty}
      close={() => !busy && close()}
    >
      <form onSubmit={save} className="form">
        <FormError error={error} />
        <label>
          账号
          <input value={name} readOnly />
        </label>
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
              {catalog.users.map((u) => (
                <option key={u.id} value={u.id}>
                  {u.name}
                </option>
              ))}
            </Select>
          </label>
        </div>
        <p className="field-hint">保存时开始计费，升级前用量不计入。</p>
        <footer>
          <button
            type="button"
            className="button"
            disabled={busy}
            onClick={close}
          >
            取消
          </button>
          <button
            className="button primary"
            disabled={busy || !account.profile_version}
          >
            {busy ? "保存中…" : "升级付费"}
          </button>
        </footer>
      </form>
    </Modal>
  );
}

function equalPercents(users: { id: number }[]): Record<number, string> {
  return Object.fromEntries(
    users.map((u, i) => [
      u.id,
      (
        (Math.floor(10000 / users.length) + (i < 10000 % users.length ? 1 : 0)) /
        100
      ).toFixed(2),
    ]),
  );
}

export function ExpenseForm({
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
  const initialName = source?.name || "";
  const initialCost = source ? costInput(source.cost_fen) : "";
  const initialIncurredOn = source?.incurred_on || inputDate().slice(0, 10);
  const initialCreditor = source?.creditor_id || catalog.users[0]?.id || 0;
  const initialMode: "equal" | "custom" = source?.allocation_mode || "equal";
  const initialMembers = Object.fromEntries(
    source
      ? (source.members || []).map((m) => [
          m.user_id,
          m.weight !== undefined ? (Number(m.weight) / 100).toFixed(2) : "",
        ])
      : catalog.users.map((u) => [u.id, ""]),
  );
  const [name, setName] = useState(initialName);
  const [cost, setCost] = useState(initialCost);
  const [incurredOn, setIncurredOn] = useState(initialIncurredOn);
  const [creditor, setCreditor] = useState(initialCreditor);
  const [mode, setMode] = useState<"equal" | "custom">(initialMode);
  const [members, setMembers] =
    useState<Record<number, string>>(initialMembers);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");
  const dirty =
    name !== initialName ||
    cost !== initialCost ||
    incurredOn !== initialIncurredOn ||
    creditor !== initialCreditor ||
    mode !== initialMode ||
    JSON.stringify(members) !== JSON.stringify(initialMembers);
  const availableUsers = [
    ...catalog.users,
    ...(source?.members || [])
      .filter((m) => !catalog.users.some((u) => u.id === m.user_id))
      .map((m) => ({
        id: m.user_id,
        name:
          expense?.allocations.find((a) => a.user_id === m.user_id)
            ?.user_name || `用户 #${m.user_id}`,
      })),
  ];
  const selected = availableUsers.filter((u) =>
    Object.prototype.hasOwnProperty.call(members, u.id),
  );
  const validRatios = selected.every((u) =>
    /^(0|[1-9][0-9]{0,2})(\.[0-9]{1,2})?$/.test(members[u.id]),
  );
  const totalBps = selected.reduce(
    (sum, u) => sum + Math.round(Number(members[u.id] || 0) * 100),
    0,
  );
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
          ...(mode === "custom"
            ? { percent: Number(members[u.id] || 0).toFixed(2) }
            : {}),
        })),
      };
      if (source)
        await api(`/expenses/${source.id}`, "PATCH", {
          ...body,
          version: source.version,
        });
      else await api("/expenses", "POST", body);
      done();
    } catch (e) {
      setError((e as Error).message);
    } finally {
      setBusy(false);
    }
  }
  return (
    <Modal
      title={source ? "修改固定费用" : "新增固定费用"}
      busy={busy}
      dirty={dirty}
      close={() => !busy && close()}
    >
      <form className="form" onSubmit={save}>
        <FormError error={error} />
        <label>
          名称
          <input
            required
            maxLength={120}
            value={name}
            onChange={(e) => setName(e.target.value)}
          />
        </label>
        <div className="form-columns">
          <label>
            金额（元）
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
            日期
            <input
              required
              type="date"
              value={incurredOn}
              onChange={(e) => setIncurredOn(e.target.value)}
            />
          </label>
        </div>
        <label>
          垫付人
          <Select
            value={creditor}
            onChange={(e) => setCreditor(Number(e.target.value))}
          >
            {source &&
              !catalog.users.some((u) => u.id === source.creditor_id) && (
                <option value={source.creditor_id}>
                  {expense?.creditor_name}（历史用户）
                </option>
              )}
            {catalog.users.map((u) => (
              <option key={u.id} value={u.id}>
                {u.name}
              </option>
            ))}
          </Select>
        </label>
        <fieldset className="member-picker">
          <legend className="picker-head">
            <span>参与人 · 已选 {selected.length}</span>
            {!!availableUsers.length && (
              <span className="picker-actions">
                <button
                  type="button"
                  className="text-button"
                  disabled={busy || selected.length === availableUsers.length}
                  onClick={() =>
                    setMembers(
                      mode === "custom"
                        ? equalPercents(availableUsers)
                        : Object.fromEntries(
                            availableUsers.map((u) => [u.id, ""]),
                          ),
                    )
                  }
                >
                  全选
                </button>
                <button
                  type="button"
                  className="text-button"
                  disabled={busy || !selected.length}
                  onClick={() => setMembers({})}
                >
                  清空
                </button>
              </span>
            )}
          </legend>
          <div className="member-list">
            {availableUsers.map((u) => {
              const checked = Object.prototype.hasOwnProperty.call(
                members,
                u.id,
              );
              return (
                <div key={u.id} className="member-row">
                  <label className="checkline">
                    <input
                      type="checkbox"
                      checked={checked}
                      onChange={(e) =>
                        setMembers((prev) => {
                          const next = { ...prev };
                          if (e.target.checked)
                            next[u.id] = mode === "custom" ? "0.00" : "";
                          else delete next[u.id];
                          return next;
                        })
                      }
                    />
                    <span>{u.name}</span>
                  </label>
                  {mode === "custom" && checked && (
                    <input
                      className="percent-input"
                      inputMode="decimal"
                      aria-label={`${u.name}比例`}
                      value={members[u.id]}
                      onChange={(e) =>
                        setMembers((prev) => ({
                          ...prev,
                          [u.id]: e.target.value,
                        }))
                      }
                    />
                  )}
                  {mode === "custom" && checked && <span>%</span>}
                </div>
              );
            })}
          </div>
        </fieldset>
        <div
          className="segments allocation-mode"
          role="group"
          aria-label="分摊方式"
        >
          <button
            type="button"
            className={mode === "equal" ? "active" : ""}
            onClick={() => setMode("equal")}
          >
            等额
          </button>
          <button
            type="button"
            className={mode === "custom" ? "active" : ""}
            onClick={() => {
              if (mode === "equal" && selected.length)
                setMembers(equalPercents(selected));
              setMode("custom");
            }}
          >
            自定义比例
          </button>
        </div>
        {mode === "custom" && (
          <p
            className={
              Math.abs(total - 100) < 0.001
                ? "field-hint valid-hint"
                : "field-hint"
            }
          >
            合计 {total.toFixed(2)}%
          </p>
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

export function Allocation({
  value,
}: {
  value: Calculation | ExpenseCalculation;
}) {
  const isExpense =
    "source_type" in value && value.source_type === "fixed_expense";
  const expenseValue = value as ExpenseCalculation;
  const cycleValue = value as Calculation;
  const title = isExpense ? expenseValue.expense.name : cycleValue.cycle.name;
  const amount = isExpense
    ? expenseValue.expense.cost_fen
    : (cycleValue.bill?.billable_cost_fen ?? cycleValue.cycle.cost_fen);
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
        <strong className="amount">
          {!isExpense && cycleValue.allocation_pending
            ? "待账单拆分"
            : money(amount)}
        </strong>
      </div>
      {!isExpense && (
        <div className="detail-facts">
          <span>
            周期总消费 <b>{consumption(cycleValue.total_consumption)}</b>
          </span>
          <span>
            调用 <b>{cycleValue.record_count.toLocaleString()}</b>
          </span>
          <span title="本次计费（元）÷ 周期原始总消费">
            {cycleValue.display?.settled ? "结算倍率" : "预估倍率"}{" "}
            <b>
              {cycleValue.display?.cost_multiplier != null
                ? `${cycleValue.display.cost_multiplier}×`
                : "—"}
            </b>
          </span>
        </div>
      )}
      {!isExpense && cycleValue.bill && !cycleValue.vendor_period && (
        <div className="detail-facts">
          <span>
            {cycleValue.bill.allocation_mode === "matched_only"
              ? "精准匹配"
              : "上游扣费分摊"}
          </span>
          <span>
            上游总扣费{" "}
            <b>
              {cycleValue.bill.manual_summary?.source === "local_only"
                ? "未提供"
                : money(
                    cycleValue.bill.upstream_total_fen ??
                      cycleValue.cycle.cost_fen,
                  )}
            </b>
          </span>
          {cycleValue.bill.manual_summary && (
            <span>
              手动补计{" "}
              <b>{billedMoney(cycleValue.bill.manual_summary.amount)}</b>
            </span>
          )}
          <span>
            本次计费 <b>{money(amount)}</b>
          </span>
          <span>
            已匹配 <b>{cycleValue.bill.matched_count} 条</b>
          </span>
          <span>
            {cycleValue.bill.allocation_mode === "matched_only"
              ? "未匹配不计费"
              : "未匹配"}{" "}
            <b>
              {cycleValue.bill.unmatched_count} 条 ·{" "}
              {billedMoney(cycleValue.bill.unmatched_amount)}
            </b>
          </span>
        </div>
      )}
      <AllocationRows allocations={value.allocations} />
    </section>
  );
}

export function AllocationRows({ allocations }: { allocations: Share[] }) {
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

export function StartConfirmation({
  calculation,
  onConfirmed,
  onStateChange,
}: {
  calculation: Calculation;
  onConfirmed: () => Promise<void>;
  onStateChange?: (state: { busy: boolean; dirty: boolean }) => void;
}) {
  const initialValue = inputDate(
    calculation.start_lower || calculation.cycle.start,
  );
  const [value, setValue] = useState(initialValue);
  const [candidate, setCandidate] = useState<{
    version: number;
    anchor: string;
    usage: { id: number; created_at: string } | null;
  } | null>(null);
  const [method, setMethod] = useState<"provided_time" | "first_usage_after">(
    "provided_time",
  );
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");
  const dirty =
    value !== initialValue || method !== "provided_time" || candidate !== null;
  const stateCallback = useRef(onStateChange);
  stateCallback.current = onStateChange;
  useEffect(() => {
    stateCallback.current?.({ busy, dirty });
  }, [busy, dirty]);
  useEffect(() => () => stateCallback.current?.({ busy: false, dirty: false }), []);
  async function findCandidate() {
    setBusy(true);
    setError("");
    setCandidate(null);
    try {
      setCandidate(
        await api(
          `/cycles/${calculation.cycle.id}/start-candidate?after=${encodeURIComponent(fromInput(value))}`,
        ),
      );
    } catch (e) {
      setError((e as Error).message);
    } finally {
      setBusy(false);
    }
  }
  async function confirm() {
    setBusy(true);
    setError("");
    try {
      await api(`/cycles/${calculation.cycle.id}/start-confirm`, "POST", {
        version:
          method === "first_usage_after"
            ? candidate!.version
            : calculation.cycle.version,
        start:
          method === "first_usage_after"
            ? candidate!.anchor
            : fromInput(value),
        method,
        ...(method === "first_usage_after"
          ? { source_usage_id: candidate!.usage!.id }
          : {}),
      });
      await onConfirmed();
    } catch (e) {
      setError((e as Error).message);
    } finally {
      setBusy(false);
    }
  }
  return (
    <div className="start-confirm">
      <div className="form-columns">
        <label>
          确认方式
          <Select
            value={method}
            disabled={busy}
            onChange={(e) => {
              setMethod(e.target.value as typeof method);
              setCandidate(null);
            }}
          >
            <option value="provided_time">直接填写准确时间</option>
            <option value="first_usage_after">充值后的首个请求</option>
          </Select>
        </label>
        <label>
          {method === "first_usage_after"
            ? "充值时间（北京时间）"
            : "准确起点（北京时间）"}
          <input
            type="datetime-local"
            step="1"
            value={value}
            onChange={(e) => {
              setValue(e.target.value);
              setCandidate(null);
            }}
          />
        </label>
      </div>
      {candidate && (
        <p className="candidate-result">
          {candidate.usage
            ? `首请求：${date(candidate.usage.created_at)}`
            : "充值时间后没有已采集请求"}
        </p>
      )}
      <div className="start-actions">
        {method === "first_usage_after" && (
          <button
            className="button"
            disabled={busy || !value}
            onClick={findCandidate}
          >
            查询首请求
          </button>
        )}
        <button
          className="button primary"
          disabled={
            busy || !value || (method === "first_usage_after" && !candidate?.usage)
          }
          onClick={confirm}
        >
          {busy ? "处理中…" : "确认起点"}
        </button>
      </div>
      <FormError error={error} />
    </div>
  );
}
