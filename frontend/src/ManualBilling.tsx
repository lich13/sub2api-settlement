import ChevronDown from "lucide-react/icons/chevron-down";
import { useEffect, useMemo, useRef, useState } from "react";
import { FormError } from "./Forms";
import { Select } from "./Select";
import { billedMoney, consumption, date, ratePattern } from "./api";
import type { LocalUnmatched, ManualBasis, ManualSummary } from "./types";

export function canonicalManualBasis(basis?: ManualBasis) {
  if (!basis || (basis.source !== "local_only" && !basis.rates.length))
    return "";
  return JSON.stringify({
    source: basis.source,
    rates: [...basis.rates]
      .filter((r) => r.rate.trim() !== "")
      .map((r) => ({ account_id: r.account_id, rate: r.rate.trim() }))
      .sort((a, b) => a.account_id - b.account_id),
    ambiguous_usage_ids: [...new Set(basis.ambiguous_usage_ids)].sort(
      (a, b) => a - b,
    ),
  });
}
export function useManualBilling(initial?: ManualBasis) {
  const [source, setSource] = useState<ManualBasis["source"]>(
    initial?.source || "upstream",
  );
  const [rates, setRates] = useState<Record<number, string>>(() =>
    Object.fromEntries(initial?.rates.map((r) => [r.account_id, r.rate]) || []),
  );
  const [selected, setSelected] = useState<number[]>(
    initial?.ambiguous_usage_ids || [],
  );
  const touched = useRef(false);
  useEffect(() => {
    if (initial && !touched.current) {
      setSource(initial.source);
      setRates(
        Object.fromEntries(initial.rates.map((r) => [r.account_id, r.rate])),
      );
      setSelected(initial.ambiguous_usage_ids);
    }
  }, [initial]);
  const activeRates = Object.entries(rates)
    .filter(([, rate]) => rate.trim() !== "")
    .map(([account_id, rate]) => ({
      account_id: Number(account_id),
      rate: rate.trim(),
    }));
  const basis: ManualBasis | undefined =
    source === "local_only" || activeRates.length > 0
      ? {
          source,
          rates: activeRates,
          ambiguous_usage_ids: selected,
        }
      : undefined;
  const valid = activeRates.every((r) => ratePattern.test(r.rate));
  return {
    source,
    rates,
    selected,
    basis,
    valid,
    touched,
    sourceChange(next: ManualBasis["source"]) {
      touched.current = true;
      setSource(next);
      setSelected([]);
    },
    rateChange(
      id: number,
      rate: string,
      dropUsageIds?: number[],
    ) {
      touched.current = true;
      setRates((prev) => ({ ...prev, [id]: rate }));
      if (!rate.trim()) {
        if (dropUsageIds && dropUsageIds.length > 0) {
          const drop = new Set(dropUsageIds);
          setSelected((prev) =>
            prev.some((x) => drop.has(x))
              ? prev.filter((x) => !drop.has(x))
              : prev,
          );
        } else if (
          !dropUsageIds &&
          initial?.rates.some((r) => r.account_id === id && r.rate.trim() !== "")
        ) {
          setSelected([]);
        }
      }
    },
    select(id: number, checked: boolean) {
      touched.current = true;
      setSelected((prev) =>
        checked ? [...new Set([...prev, id])] : prev.filter((x) => x !== id),
      );
    },
    setSelectedIds(ids: number[]) {
      touched.current = true;
      setSelected([...new Set(ids)]);
    },
    resetSelection() {
      setSelected([]);
    },
  };
}
export function ManualBillingEditor({
  manual,
  accounts,
  summary,
  ambiguous = [],
  changed,
  onResetAmbiguous,
  disabled = false,
}: {
  manual: ReturnType<typeof useManualBilling>;
  accounts: { account_id: number; name: string }[];
  summary?: ManualSummary;
  ambiguous?: LocalUnmatched[];
  changed: () => void;
  onResetAmbiguous?: () => void;
  disabled?: boolean;
}) {
  const [limit, setLimit] = useState(50);
  const selectedSet = useMemo(() => new Set(manual.selected), [manual.selected]);
  const summaryByAccount = useMemo(
    () => new Map((summary?.accounts || []).map((s) => [s.account_id, s])),
    [summary],
  );
  const accountNameById = useMemo(
    () => new Map(accounts.map((a) => [a.account_id, a.name])),
    [accounts],
  );
  const ambiguousIdsByAccount = useMemo(() => {
    const map = new Map<number, number[]>();
    for (const r of ambiguous) {
      let list = map.get(r.account_id);
      if (!list) {
        list = [];
        map.set(r.account_id, list);
      }
      list.push(r.usage_id);
    }
    return map;
  }, [ambiguous]);
  const selectableIds = useMemo(
    () =>
      ambiguous
        .filter((r) => manual.rates[r.account_id]?.trim())
        .map((r) => r.usage_id),
    [ambiguous, manual.rates],
  );
  const allAmbiguousSelected =
    selectableIds.length > 0 &&
    selectableIds.every((id) => selectedSet.has(id));
  return (
    <section className="manual-billing">
      <label>
        账单来源
        <Select
          disabled={disabled}
          value={manual.source}
          onChange={(e) => {
            manual.sourceChange(e.target.value as ManualBasis["source"]);
            onResetAmbiguous?.();
            changed();
          }}
        >
          <option value="upstream">使用上游账单</option>
          <option value="local_only">仅用本地记录</option>
        </Select>
      </label>
      <div className="manual-rates">
        {accounts.map((a) => {
          const accountSummary = summaryByAccount.get(a.account_id);
          return (
            <div className="manual-rate" key={a.account_id}>
              <label>
                <span>{a.name}</span>
                <input
                  aria-label={`${a.name} 补计倍率`}
                  disabled={disabled}
                  inputMode="decimal"
                  placeholder="不补计"
                  value={manual.rates[a.account_id] || ""}
                  onChange={(e) => {
                    manual.rateChange(
                      a.account_id,
                      e.target.value,
                      ambiguous.length > 0
                        ? ambiguousIdsByAccount.get(a.account_id) || []
                        : undefined,
                    );
                    changed();
                  }}
                />
              </label>
              {accountSummary && (
                <p className="field-hint">
                  {accountSummary.request_count} 条 · 原始消费{" "}
                  {consumption(accountSummary.local_amount)} · 补计{" "}
                  {billedMoney(accountSummary.amount)} · 排除{" "}
                  {accountSummary.excluded_count} 条
                </p>
              )}
            </div>
          );
        })}
      </div>
      <p className="field-hint">
        本地原始消费 × 倍率；留空不补计，填写任一账号倍率即启用补计，0
        表示不计费。最多六位小数。
      </p>
      {!manual.valid && (
        <FormError error="倍率须为非负数，最多六位小数" />
      )}
      {!!ambiguous.length && (
        <details className="manual-ambiguous">
          <summary>
            歧义请求 · {ambiguous.length} 条
            <ChevronDown size={16} />
          </summary>
          <div className="picker-head">
            <p className="field-hint">
              核对后选择；明确匹配的请求不会重复补计。
            </p>
            <div className="picker-actions">
              <button
                type="button"
                className="text-button"
                disabled={disabled || !selectableIds.length || allAmbiguousSelected}
                onClick={() => {
                  manual.setSelectedIds([...manual.selected, ...selectableIds]);
                  changed();
                }}
              >
                全选
              </button>
              <button
                type="button"
                className="text-button"
                disabled={disabled || !manual.selected.length}
                onClick={() => {
                  manual.setSelectedIds([]);
                  changed();
                }}
              >
                清空
              </button>
            </div>
          </div>
          <p className="field-hint">
            歧义费用可能已包含在上游总额中，勾选前请核对，系统不能自动去重。
          </p>
          {ambiguous.slice(0, limit).map((r) => (
            <label className="checkline manual-request" key={r.usage_id}>
              <input
                aria-label={`补计请求 ${r.usage_id}`}
                type="checkbox"
                disabled={disabled || !manual.rates[r.account_id]?.trim()}
                checked={selectedSet.has(r.usage_id)}
                onChange={(e) => {
                  manual.select(r.usage_id, e.target.checked);
                  changed();
                }}
              />
              <span>
                {accountNameById.get(r.account_id)}
                <small>
                  {date(r.created_at)} · {r.model || "模型未记录"}
                </small>
                <small>
                  原始消费 {consumption(r.local_amount)} · 匹配歧义
                </small>
              </span>
            </label>
          ))}
          {limit < ambiguous.length && (
            <button
              type="button"
              className="text-button"
              onClick={() => setLimit((n) => n + 50)}
            >
              显示更多
            </button>
          )}
        </details>
      )}
    </section>
  );
}
