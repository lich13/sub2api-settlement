import { useEffect, useRef, useState } from "react";
import { ChevronDown } from "lucide-react";
import {
  api,
  downloadCsv,
  ApiError,
  money,
  costInput,
  inputDate,
  fromInput,
  amountPattern,
  billedMoney,
} from "./api";
import type {
  Catalog,
  Cycle,
  Preview,
  Calculation,
  BillPreview,
  CostKind,
  BillMode,
  DeletionJob,
  VendorPeriod,
} from "./types";
import { useAutoPreview } from "./useAutoPreview";
import { VendorBillPanel } from "./Vendors";
import { Select } from "./Select";
import { Modal } from "./Modal";
import {
  Allocation,
  AllocationRows,
  FormError,
  StartConfirmation,
} from "./Forms";
import { DeletionScope, type DeletionScopeValue } from "./DeletionScope";

function BillAllocation({
  cycle,
  preview,
}: {
  cycle: Cycle;
  preview: BillPreview;
}) {
  return (
    <section className="allocation bill-allocation">
      <div className="allocation-head">
        <div>
          <strong>{cycle.name}</strong>
          <span>上游账单分摊</span>
        </div>
        <strong className="amount">
          {money(preview.billable_cost_fen ?? preview.cost_fen)}
        </strong>
      </div>
      <AllocationRows allocations={preview.allocations} />
    </section>
  );
}

export function BillImportPanel({
  cycle,
  calculation,
  end,
  onConfirmed,
  onDraftChange,
}: {
  cycle: Cycle;
  calculation?: Calculation;
  end: string;
  onConfirmed: () => Promise<void>;
  onDraftChange?: (dirty: boolean) => void;
}) {
  const [csv, setCsv] = useState("");
  const [fileName, setFileName] = useState("");
  const [preview, setPreview] = useState<BillPreview | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");
  const effectiveCycle =
    calculation && calculation.cycle.version >= cycle.version
      ? calculation.cycle
      : cycle;
  const startConfirmed = calculation
    ? calculation.start_confirmed !== false
    : effectiveCycle.start_confirmed !== false;
  const [open, setOpen] = useState(
    effectiveCycle.cost_source === "upstream_bill" ||
      effectiveCycle.cost_source === "manual_bill",
  );
  const [mode, setMode] = useState<BillMode>(
    effectiveCycle.bill_allocation_mode || "proportional",
  );
  const [committedMode, setCommittedMode] = useState<BillMode>(
    effectiveCycle.bill_allocation_mode || "proportional",
  );
  function invalidate() {
    ++requestVersion.current;
    setPreview(null);
    setBusy(false);
  }
  const requestVersion = useRef(0);
  const fileRead = useRef(0);
  const csvInput = useRef<HTMLInputElement>(null);
  const draftCallback = useRef(onDraftChange);
  draftCallback.current = onDraftChange;
  const hasDraft =
    preview !== null ||
    csv.trim() !== "" ||
    (!!effectiveCycle.bill_import_id && mode !== committedMode);
  useEffect(() => {
    draftCallback.current?.(hasDraft);
  }, [hasDraft]);
  useEffect(() => () => draftCallback.current?.(false), []);

  useEffect(() => {
    const nextMode = effectiveCycle.bill_allocation_mode || "proportional";
    setMode(nextMode);
    setCommittedMode(nextMode);
    if (
      effectiveCycle.cost_source === "upstream_bill" ||
      effectiveCycle.cost_source === "manual_bill"
    ) {
      setOpen(true);
    }
  }, [
    effectiveCycle.id,
    effectiveCycle.bill_allocation_mode,
    effectiveCycle.cost_source,
  ]);

  useEffect(() => {
    ++requestVersion.current;
    setPreview(null);
    setBusy(false);
    setError("");
  }, [end, effectiveCycle.id, effectiveCycle.version]);
  useEffect(
    () => () => {
      ++requestVersion.current;
      ++fileRead.current;
    },
    [],
  );

  async function inspect(useExisting = false, selectedMode = mode) {
    if (!startConfirmed) {
      setError("请先确认周期起点后再匹配账单");
      return;
    }
    if (!end) {
      setError("请填写有效的结束时间");
      return;
    }
    if (!useExisting && !csv.trim()) {
      setError("请选择 CSV 文件");
      return;
    }
    setBusy(true);
    setError("");
    const token = ++requestVersion.current;
    try {
      const result = await api<BillPreview>(
        `/cycles/${effectiveCycle.id}/bill-previews`,
        "POST",
        {
          version: effectiveCycle.version,
          end,
          allocation_mode: selectedMode,
          ...(useExisting
            ? { import_id: effectiveCycle.bill_import_id }
            : { csv, file_name: fileName || "usage.csv" }),
        },
      );
      if (token !== requestVersion.current) return;
      setPreview(result);
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
    setBusy(true);
    setError("");
    try {
      await api(`/cycles/${effectiveCycle.id}/bill-confirm`, "POST", {
        version: effectiveCycle.version,
        preview_id: preview.id,
      });
      setCommittedMode(mode);
      setCsv("");
      setFileName("");
      if (csvInput.current) csvInput.current.value = "";
      setPreview(null);
      await onConfirmed();
    } catch (e) {
      setError((e as Error).message);
    } finally {
      setBusy(false);
    }
  }
  async function useLocalConsumption() {
    invalidate();
    ++fileRead.current;
    if (
      effectiveCycle.cost_source !== "upstream_bill" &&
      effectiveCycle.cost_source !== "manual_bill"
    ) {
      setCsv("");
      setFileName("");
      if (csvInput.current) csvInput.current.value = "";
      setMode(committedMode);
      setError("");
      return;
    }
    const token = requestVersion.current;
    setBusy(true);
    setError("");
    try {
      await api(`/cycles/${effectiveCycle.id}`, "PATCH", {
        account_id: effectiveCycle.account_id,
        version: effectiveCycle.version,
        name: effectiveCycle.name,
        cost: costInput(effectiveCycle.cost_fen),
        creditor_id: effectiveCycle.creditor_id,
        start: effectiveCycle.start,
        start_mode: effectiveCycle.start_mode || "exact",
        cost_kind: "exact",
        cost_source: "manual",
      });
      if (token !== requestVersion.current) return;
      setCsv("");
      setFileName("");
      if (csvInput.current) csvInput.current.value = "";
      setMode("proportional");
      setCommittedMode("proportional");
      await onConfirmed();
    } catch (e) {
      if (token === requestVersion.current) setError((e as Error).message);
    } finally {
      if (token === requestVersion.current) setBusy(false);
    }
  }
  return (
    <details
      className="bill-import"
      open={open}
      onToggle={(e) => setOpen(e.currentTarget.open)}
    >
      <summary className="bill-import-summary">
        <div>
          <strong>上游账单（可选）</strong>
          <span>
            {effectiveCycle.name} ·{" "}
            {effectiveCycle.cost_source === "upstream_bill"
              ? "替换账单"
              : "默认按本地消费分摊"}
          </span>
        </div>
        <ChevronDown size={16} />
      </summary>
      <p className="field-hint">
        本期总成本 {money(effectiveCycle.cost_fen)}
        ，可直接按本地原始消费占比分摊，无需上传账单或填写倍率。
      </p>
      <button
        type="button"
        className="button"
        disabled={
          busy ||
          effectiveCycle.cost_kind === "estimated" ||
          (effectiveCycle.cost_source !== "upstream_bill" &&
            effectiveCycle.cost_source !== "manual_bill" &&
            !hasDraft)
        }
        onClick={useLocalConsumption}
      >
        按本地消费比例分摊
      </button>
      {!startConfirmed && (
        <p className="field-hint warning-text">
          请先确认周期起点后再匹配账单
        </p>
      )}
      <label>
        分摊方式
        <Select
          value={mode}
          disabled={busy}
          onChange={(e) => {
            const next = e.target.value as BillMode;
            const hasSource =
              !!csv.trim() || !!effectiveCycle.bill_import_id;
            setMode(next);
            invalidate();
            if (hasSource) void inspect(!csv.trim(), next);
          }}
        >
          <option value="proportional">比例分摊</option>
          <option value="matched_only">精准匹配</option>
        </Select>
      </label>
      <label>
        CSV 文件
        <input
          ref={csvInput}
          type="file"
          accept=".csv,text/csv"
          disabled={busy}
          onChange={async (e) => {
            const file = e.target.files?.[0];
            const token = ++fileRead.current;
            invalidate();
            if (!file) {
              setFileName("");
              setCsv("");
              return;
            }
            if (file.size > 10 * 1024 * 1024) {
              setError("CSV 不能超过 10 MB");
              return;
            }
            const content = await file.text();
            if (token !== fileRead.current) return;
            setFileName(file.name);
            setCsv(content);
          }}
        />
      </label>
      <div className="start-actions">
        <button
          type="button"
          className="button"
          disabled={busy || !startConfirmed || !csv.trim() || !end}
          onClick={() => inspect()}
        >
          {busy && !preview ? "匹配中…" : "账单预览"}
        </button>
        {!!effectiveCycle.bill_import_id && (
          <button
            type="button"
            className="button"
            disabled={busy || !startConfirmed}
            onClick={() => inspect(true)}
          >
            沿用已导入账单
          </button>
        )}
      </div>
      {preview && (
        <div className="bill-stats">
          {preview.file_name && (
            <div>
              <span>文件</span>
              <b>{preview.file_name}</b>
            </div>
          )}
          <div>
            <span>上游总扣费</span>
            <b>
              {preview.manual_basis?.source === "local_only"
                ? "未提供"
                : money(preview.upstream_total_fen ?? preview.cost_fen)}
            </b>
          </div>
          {preview.manual_summary && (
            <div>
              <span>手动补计</span>
              <b>{billedMoney(preview.manual_summary.amount)}</b>
            </div>
          )}
          <div>
            <span>本次计费</span>
            <b>{money(preview.billable_cost_fen ?? preview.cost_fen)}</b>
          </div>
          <div>
            <span>已匹配</span>
            <b>
              {preview.matched_count} 条 · {billedMoney(preview.matched_amount)}
            </b>
          </div>
          <div>
            <span>
              {preview.allocation_mode === "matched_only"
                ? "未匹配不计费"
                : "未匹配"}
            </span>
            <b>
              {preview.unmatched_count} 条 ·{" "}
              {billedMoney(preview.unmatched_amount)}
            </b>
          </div>
          {preview.coverage_warning && (
            <FormError error={preview.coverage_warning} />
          )}
          {!!preview.local_unmatched_count && (
            <div>
              <span>本地未列入账单</span>
              <b>{preview.local_unmatched_count} 条</b>
            </div>
          )}
          {!!preview.outside_count && (
            <div>
              <span>范围外（已自动排除）</span>
              <b>
                {preview.outside_count} 条 ·{" "}
                {billedMoney(preview.outside_amount)}
              </b>
            </div>
          )}
          {(preview.unmatched_count > 0 ||
            preview.outside_count > 0 ||
            (preview.local_unmatched?.length || 0) > 0) && (
            <button
              type="button"
              className="text-button"
              disabled={busy}
              onClick={async () => {
                const token = requestVersion.current;
                setBusy(true);
                setError("");
                try {
                  await downloadCsv(
                    `/cycles/${effectiveCycle.id}/bill-previews/${preview.id}/unmatched.csv`,
                    "未匹配账单.csv",
                  );
                } catch (e) {
                  if (token === requestVersion.current)
                    setError((e as Error).message);
                } finally {
                  if (token === requestVersion.current) setBusy(false);
                }
              }}
            >
              导出未匹配明细
            </button>
          )}
          <BillAllocation cycle={effectiveCycle} preview={preview} />
          {!preview.can_confirm && (
            <FormError error="请确认起点、消费覆盖及正金额计费依据" />
          )}
          <button
            type="button"
            className="button primary"
            disabled={busy || !preview.can_confirm}
            onClick={confirm}
          >
            {busy
              ? "确认中…"
              : preview.manual_basis
                ? "确认计费依据"
                : preview.allocation_mode === "matched_only"
                  ? "确认仅按匹配金额计费"
                  : "确认替换成本"}
          </button>
        </div>
      )}
      {error && <div className="form-error">{error}</div>}
    </details>
  );
}

export function SettleForm({
  ids,
  expenseIds = [],
  expenseRevision = "",
  cycles = [],
  vendorPeriods = [],
  catalog,
  done,
  close,
  renewByDefault = false,
}: {
  ids: number[];
  expenseIds?: number[];
  expenseRevision?: string;
  cycles?: Cycle[];
  vendorPeriods?: VendorPeriod[];
  catalog: Catalog;
  done: (message?: string) => void;
  close: () => void;
  renewByDefault?: boolean;
}) {
  const [end, setEnd] = useState(() => {
    const confirmedEnds = new Set<string>();
    for (const c of cycles) {
      if (!c.vendor_period_id && c.bill_end) confirmedEnds.add(c.bill_end);
    }
    for (const p of vendorPeriods) {
      if (p.members?.some((m) => ids.includes(m.id)) && p.bill_end) {
        confirmedEnds.add(p.bill_end);
      }
    }
    if (confirmedEnds.size === 1) {
      return inputDate([...confirmedEnds][0]);
    }
    return inputDate();
  });
  const [touched, setTouched] = useState(false);
  const [displayPreview, setDisplayPreview] = useState<Preview | null>(null);
  const [renew, setRenew] = useState<
    Record<number, { cost: string; creditor_id: number; cost_kind: CostKind }>
  >({});
  const [vendorRenew, setVendorRenew] = useState<
    Record<number, { cost: string; creditor_id: number }>
  >({});
  const renewalDefaultsSet = useRef(false);
  const [deleteIds, setDeleteIds] = useState<number[]>([]);
  const [billDrafts, setBillDrafts] = useState<Record<string, boolean>>({});
  const hasBillDraft = Object.values(billDrafts).some(Boolean);
  const [deleteScopes, setDeleteScopes] = useState<
    Record<number, DeletionScopeValue | null>
  >({});
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");
  const {
    preview,
    loading,
    error: previewError,
    refresh: inspect,
  } = useAutoPreview({
    ids,
    expenseIds,
    end,
    paused: busy,
    revision:
      JSON.stringify(cycles.map((c) => [c.id, c.version])) +
      expenseRevision +
      JSON.stringify(vendorPeriods.map((p) => [p.id, p.version])),
    onResult(next) {
      setDisplayPreview(next);
      if (renewByDefault && !renewalDefaultsSet.current) {
        setRenew(
          Object.fromEntries(
            next.cycles
              .filter(
                (c) => !c.cycle.account_deleted && !c.cycle.vendor_period_id,
              )
              .map((c) => [
                c.cycle.id,
                {
                  cost: costInput(c.cycle.cost_fen),
                  creditor_id: c.cycle.creditor_id,
                  cost_kind: "exact",
                },
              ]),
          ),
        );
        setVendorRenew(
          Object.fromEntries(
            next.cycles
              .filter((c) => c.vendor_period)
              .map((c) => [
                c.vendor_period!.id,
                {
                  cost: costInput(c.vendor_period!.cost_fen),
                  creditor_id: c.vendor_period!.creditor_id,
                },
              ]),
          ),
        );
        renewalDefaultsSet.current = true;
      }
    },
  });
  const pendingSubmission = useRef<{
    preview_id: string;
    renewals: {
      cycle_id: number;
      cost: string;
      creditor_id: number;
      cost_kind: CostKind;
    }[];
    delete_cycle_ids?: number[];
    delete_scopes?: Record<string, string>;
    vendor_renewals?: {
      period_id: number;
      version: number;
      cost: string;
      creditor_id: number;
    }[];
  } | null>(null);
  let cutoff = "";
  try {
    if (end) cutoff = fromInput(end);
  } catch {
    /* Date input may be incomplete. */
  }
  const cycleMap = new Map<number, Cycle>();
  for (const cycle of cycles) cycleMap.set(cycle.id, cycle);
  for (const calculation of displayPreview?.cycles || []) {
    const current = cycleMap.get(calculation.cycle.id);
    if (!current || current.version <= calculation.cycle.version)
      cycleMap.set(calculation.cycle.id, calculation.cycle);
  }
  for (const [id, cycle] of cycleMap) {
    const name = catalog.accounts.find((a) => a.id === cycle.account_id)?.name;
    if (name) cycleMap.set(id, { ...cycle, name });
  }
  const billCycles = [...cycleMap.values()].filter((c) => !c.vendor_period_id);
  const groupMap = new Map(
    vendorPeriods
      .filter((p) => p.members?.some((c) => ids.includes(c.id)))
      .map((p) => [p.id, p]),
  );
  for (const c of displayPreview?.cycles || [])
    if (
      c.vendor_period &&
      (!groupMap.has(c.vendor_period.id) ||
        groupMap.get(c.vendor_period.id)!.version <= c.vendor_period.version)
    )
      groupMap.set(c.vendor_period.id, c.vendor_period);
  const groups = [...groupMap.values()];
  const calculations = new Map(
    (preview?.cycles || []).map((calculation) => [
      calculation.cycle.id,
      calculation,
    ]),
  );
  const unconfirmedStarts = (displayPreview?.cycles || []).filter(
    (c) =>
      !c.cycle.vendor_period_id &&
      !c.start_confirmed &&
      (c.start_mode === "date_estimate" || c.start_mode === "minute_estimate"),
  );
  async function settle() {
    if (hasBillDraft) {
      setError("请先预览并确认已修改的计费依据");
      return;
    }
    if (
      !pendingSubmission.current &&
      deleteIds.some((id) => !deleteScopes[id])
    ) {
      setError("请先核验删除范围");
      return;
    }
    if ((!preview || loading) && !pendingSubmission.current) return;
    if (
      !pendingSubmission.current &&
      preview &&
      preview.cycles.some((c) => c.can_settle === false)
    ) {
      setError("请先确认起点、成本或账单");
      return;
    }
    if (
      !pendingSubmission.current &&
      preview &&
      Date.parse(preview.expires_at) <= Date.now()
    ) {
      await inspect();
      return;
    }
    if (
      !pendingSubmission.current &&
      [...Object.values(renew), ...Object.values(vendorRenew)].some(
        (x) => !amountPattern.test(x.cost),
      )
    ) {
      setError("续期成本最多保留两位小数");
      return;
    }
    if (
      !pendingSubmission.current &&
      groups.some(
        (g) =>
          vendorRenew[g.id] &&
          ![...cycleMap.values()].some(
            (c) =>
              c.vendor_period_id === g.id &&
              !c.account_deleted &&
              !deleteIds.includes(c.id),
          ),
      )
    ) {
      setError("没有可续费成员，请取消整组续费");
      return;
    }
    setBusy(true);
    setError("");
    try {
      const payload = pendingSubmission.current || {
        preview_id: preview!.id,
        renewals: Object.entries(renew)
          .filter(([id]) => !cycleMap.get(Number(id))?.vendor_period_id)
          .map(([id, r]) => ({
            cycle_id: Number(id),
            ...r,
          })),
        ...(groups.length
          ? {
              vendor_renewals: groups
                .filter((g) => vendorRenew[g.id])
                .map((g) => ({
                  period_id: g.id,
                  version: g.version,
                  cost: vendorRenew[g.id]?.cost ?? costInput(g.cost_fen),
                  creditor_id: vendorRenew[g.id]?.creditor_id ?? g.creditor_id,
                })),
            }
          : {}),
        ...(deleteIds.length
          ? {
              delete_cycle_ids: deleteIds,
              delete_scopes: Object.fromEntries(
                deleteIds.map((id) => [id, deleteScopes[id]!.digest]),
              ),
            }
          : {}),
      };
      pendingSubmission.current = payload;
      const result = await api<{ deletions?: DeletionJob[] }>(
        "/settlements",
        "POST",
        payload,
      );
      pendingSubmission.current = null;
      done(
        result.deletions?.some((j) =>
          ["failed", "verify", "cancelled"].includes(j.status),
        )
          ? "已出单，删除未完成。请在管理视图中查看。"
          : result.deletions?.some((j) => j.status !== "done")
            ? "已出单，正在删除账号"
            : result.deletions?.length
              ? "已出单并删除账号"
              : undefined,
      );
    } catch (e) {
      setError((e as Error).message);
      if (e instanceof ApiError && e.status < 500)
        pendingSubmission.current = null;
    } finally {
      setBusy(false);
    }
  }
  return (
    <Modal
      title={`结算预览 · ${ids.length + expenseIds.length} 项`}
      wide
      busy={busy}
      dirty={touched || hasBillDraft || !!pendingSubmission.current}
      close={() => !busy && close()}
    >
      <div className="form">
        <FormError error={error} />
        <FormError error={previewError} />
        {previewError && (
          <button className="button" disabled={busy} onClick={() => inspect()}>
            重试
          </button>
        )}
        {loading && (
          <p className="preview-progress" role="status">
            正在核算分摊…
          </p>
        )}
        <div className="end-picker">
          {!!ids.length && (
            <label>
              结束时间（北京时间）
              <input
                type="datetime-local"
                step="1"
                value={end}
                disabled={busy || !!pendingSubmission.current}
                onChange={(e) => {
                  setTouched(true);
                  setEnd(e.target.value);
                }}
              />
            </label>
          )}
        </div>
        <fieldset
          className="settlement-fields"
          disabled={busy || !!pendingSubmission.current}
        >
          {unconfirmedStarts.map((c) => (
            <div key={`start-${c.cycle.id}`}>
              <strong>
                {cycleMap.get(c.cycle.id)?.name || c.cycle.name} · 确认周期起点
              </strong>
              <StartConfirmation
                calculation={c}
                onConfirmed={() => inspect()}
              />
            </div>
          ))}
          {groups.map((g) => (
            <div key={g.id}>
              <VendorBillPanel
                onDraftChange={(dirty) =>
                  setBillDrafts((prev) => ({ ...prev, [`group-${g.id}`]: dirty }))
                }
                period={g}
                cycles={[...cycleMap.values()].filter(
                  (c) => c.vendor_period_id === g.id,
                )}
                end={
                  [...cycleMap.values()].find(
                    (c) => c.vendor_period_id === g.id && c.end,
                  )?.end || cutoff
                }
                onConfirmed={() => inspect()}
              />
              <label className="checkline renewal">
                <input
                  type="checkbox"
                  checked={!!vendorRenew[g.id]}
                  onChange={(e) => {
                    setTouched(true);
                    setVendorRenew((prev) => {
                      const next = { ...prev };
                      if (e.target.checked)
                        next[g.id] = {
                          cost: costInput(g.cost_fen),
                          creditor_id: g.creditor_id,
                        };
                      else delete next[g.id];
                      return next;
                    });
                  }}
                />
                结算后整组续费
              </label>
              {vendorRenew[g.id] && (
                <div className="form-columns vendor-renewal">
                  <label>
                    下期组成本（元）
                    <input
                      inputMode="decimal"
                      value={vendorRenew[g.id]?.cost ?? costInput(g.cost_fen)}
                      onChange={(e) => {
                        setTouched(true);
                        setVendorRenew((prev) => ({
                          ...prev,
                          [g.id]: {
                            creditor_id:
                              prev[g.id]?.creditor_id ?? g.creditor_id,
                            cost: e.target.value,
                          },
                        }));
                      }}
                    />
                  </label>
                  <label>
                    下期统一垫付人
                    <Select
                      value={vendorRenew[g.id]?.creditor_id ?? g.creditor_id}
                      onChange={(e) => {
                        setTouched(true);
                        setVendorRenew((prev) => ({
                          ...prev,
                          [g.id]: {
                            cost: prev[g.id]?.cost ?? costInput(g.cost_fen),
                            creditor_id: Number(e.target.value),
                          },
                        }));
                      }}
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
          {!!billCycles.length && (
            <div className="bill-import-list">
              {billCycles.map((cycle) => (
                <BillImportPanel
                  key={cycle.id}
                  onDraftChange={(dirty) =>
                    setBillDrafts((prev) => ({
                      ...prev,
                      [`cycle-${cycle.id}`]: dirty,
                    }))
                  }
                  cycle={cycle}
                  calculation={calculations.get(cycle.id)}
                  end={cutoff}
                  onConfirmed={() => inspect()}
                />
              ))}
            </div>
          )}
          {displayPreview?.cycles.map((c) => (
            <div key={c.cycle.id}>
              <Allocation
                value={{
                  ...c,
                  cycle: {
                    ...c.cycle,
                    name: cycleMap.get(c.cycle.id)?.name || c.cycle.name,
                  },
                }}
              />
              {c.start_warning && c.start_warning !== "周期起点未确认" && (
                <div className="form-error">{c.start_warning}</div>
              )}
              {c.cost_warning && <FormError error={c.cost_warning} />}
              {!c.cycle.vendor_period_id && (
                <label className="checkline renewal">
                  <input
                    type="checkbox"
                    checked={!!renew[c.cycle.id]}
                    disabled={
                      deleteIds.includes(c.cycle.id) || !!c.cycle.account_deleted
                    }
                    onChange={(e) => {
                      setTouched(true);
                      setRenew((prev) => {
                        const next = { ...prev };
                        if (e.target.checked)
                          next[c.cycle.id] = {
                            cost: costInput(c.cycle.cost_fen),
                            creditor_id: c.cycle.creditor_id,
                            cost_kind: "exact",
                          };
                        else delete next[c.cycle.id];
                        return next;
                      });
                    }}
                  />
                  结算后续期
                </label>
              )}
              <label className="checkline deletion-option">
                <input
                  type="checkbox"
                  checked={deleteIds.includes(c.cycle.id)}
                  disabled={!!renew[c.cycle.id] || !!c.cycle.account_deleted}
                  onChange={(e) => {
                    setTouched(true);
                    setDeleteIds((prev) =>
                      e.target.checked
                        ? [...prev, c.cycle.id]
                        : prev.filter((id) => id !== c.cycle.id),
                    );
                  }}
                />
                出单后删除账号
              </label>
              {deleteIds.includes(c.cycle.id) && (
                <DeletionScope
                  accountId={c.cycle.account_id}
                  onConfirmed={(value) =>
                    setDeleteScopes((prev) => ({
                      ...prev,
                      [c.cycle.id]: value,
                    }))
                  }
                />
              )}
              {renew[c.cycle.id] && !c.cycle.vendor_period_id && (
                <div className="form-columns">
                  <label>
                    新周期成本（元）
                    <input
                      inputMode="decimal"
                      value={renew[c.cycle.id].cost}
                      onChange={(e) => {
                        setTouched(true);
                        setRenew({
                          ...renew,
                          [c.cycle.id]: {
                            ...renew[c.cycle.id],
                            cost: e.target.value,
                          },
                        });
                      }}
                    />
                  </label>
                  <label>
                    垫付人
                    <Select
                      value={renew[c.cycle.id].creditor_id}
                      onChange={(e) => {
                        setTouched(true);
                        setRenew({
                          ...renew,
                          [c.cycle.id]: {
                            ...renew[c.cycle.id],
                            creditor_id: Number(e.target.value),
                          },
                        });
                      }}
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
            disabled={
              hasBillDraft ||
              busy ||
              (!pendingSubmission.current &&
                (loading ||
                  !preview ||
                  preview.cycles.some((c) => c.can_settle === false) ||
                  deleteIds.some((id) => !deleteScopes[id])))
            }
            onClick={settle}
          >
            {busy
              ? "结算中…"
              : pendingSubmission.current
                ? "重试结算"
                : "确认结算"}
          </button>
        </footer>
      </div>
    </Modal>
  );
}
