import { memo, useMemo } from "react";
import ChevronRight from "lucide-react/icons/chevron-right";
import CircleAlert from "lucide-react/icons/circle-alert";
import Edit3 from "lucide-react/icons/edit-3";
import { date, money } from "./api";
import { Empty, Status } from "./Forms";
import type { Account, Cycle, ExpenseCalculation } from "./types";

export const AccountsTable = memo(function AccountsTable({
  accounts,
  isAdmin,
  syncError,
  hasActiveVendors,
  selected,
  selectableCycleIds,
  allVisibleSelected,
  onToggleAll,
  onToggle,
  onConfirmStart,
  onUpgrade,
  onEditCycle,
  onSettle,
  onDetail,
}: {
  accounts: Account[];
  isAdmin: boolean;
  syncError: string | null;
  hasActiveVendors: boolean;
  selected: number[];
  selectableCycleIds: number[];
  allVisibleSelected: boolean;
  onToggleAll: () => void;
  onToggle: (id: number) => void;
  onConfirmStart: (cycle: Cycle) => void;
  onUpgrade: (account: Account) => void;
  onEditCycle: (account: Account, cycle?: Cycle) => void;
  onSettle: (ids: number[], renew?: boolean) => void;
  onDetail: (cycleId: number) => void;
}) {
  const selectedSet = useMemo(() => new Set(selected), [selected]);
  return (
    <div className="table-shell">
      <table className="accounts-table">
        <thead>
          <tr>
            {isAdmin && (
              <th className="check-cell">
                <input
                  type="checkbox"
                  aria-label="全选当前可结算项"
                  disabled={!selectableCycleIds.length}
                  checked={allVisibleSelected}
                  onChange={onToggleAll}
                />
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
          {accounts.map((a) => (
            <tr
              key={a.id}
              className={a.kind !== "paid" ? "compact-account" : undefined}
            >
              {isAdmin && (
                <td className="check-cell">
                  {a.kind === "paid" && a.cycle?.status === "open" && (
                    <input
                      aria-label={`选择 ${a.name}`}
                      type="checkbox"
                      checked={!!a.cycle && selectedSet.has(a.cycle.id)}
                      onChange={() => a.cycle && onToggle(a.cycle.id)}
                    />
                  )}
                </td>
              )}
              <td data-label="账号" className="account-name">
                <div className="account-title">
                  <span className={"platform" + (a.platform ? " " + a.platform : "")}>
                    {(a.platform || "?").slice(0, 1).toUpperCase()}
                  </span>
                  <div>
                    <strong>{a.name}</strong>
                    <small>
                      {a.kind === "free"
                        ? "免费资源"
                        : a.kind === "paid" && a.cycle
                          ? `${!a.start_confirmed && a.start_mode === "date_estimate" ? "约 " + date(a.start_lower) + " — " + date(a.start_upper) : !a.start_confirmed && a.start_mode === "minute_estimate" ? "约 " + date(a.start_lower, "minute") : date(a.cycle.start)}${a.cycle.end ? " — " + date(a.cycle.end) : " 起"}`
                          : a.platform || "—"}
                    </small>
                  </div>
                </div>
              </td>
              <td data-label="成本" className="numeric amount">
                {a.kind === "free" ? (
                  money(0)
                ) : a.kind === "paid" && a.cycle ? (
                  <>
                    <span className="cost-kind">
                      {a.cycle.cost_kind === "estimated"
                        ? "待重新配置"
                        : "准确成本"}
                    </span>
                    {a.allocation_pending ? "待账单拆分" : money(a.cycle.cost_fen)}
                  </>
                ) : (
                  "—"
                )}
              </td>
              <td data-label="垫付人">
                {a.kind !== "paid" ? "—" : a.creditor_name || "—"}
              </td>
              <td data-label="我的占比" className="numeric">
                {a.kind !== "paid" ||
                !a.cycle ||
                a.data_error ||
                a.allocation_pending ? (
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
                    : a.allocation_pending
                      ? "待账单拆分"
                      : a.kind !== "paid" ||
                          !a.cycle ||
                          a.data_error ||
                          (syncError && a.cycle.status === "open")
                        ? "—"
                        : money(a.mine?.amount_fen || 0)}
                </strong>
                {a.mine?.self_covered && a.kind !== "free" && (
                  <small>自行承担</small>
                )}
              </td>
              <td data-label="状态">
                {a.kind === "unconfigured" ? (
                  <span className="status">待配置</span>
                ) : a.kind === "free" ? (
                  <span className="status">免费</span>
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
                  <span className="status warning">
                    <CircleAlert size={12} />
                    {a.cost_warning}
                  </span>
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
                    {a.cycle?.status === "open" && !a.start_confirmed && (
                      <button
                        className="text-button"
                        onClick={() => onConfirmStart(a.cycle!)}
                      >
                        确认起点
                      </button>
                    )}
                    {a.kind === "free" ? (
                      <button
                        className="text-button primary-text"
                        onClick={() => onUpgrade(a)}
                      >
                        升级付费
                      </button>
                    ) : (
                      <button
                        className="text-button"
                        onClick={() =>
                          onEditCycle(
                            a,
                            a.cycle?.status === "open" ? a.cycle : undefined,
                          )
                        }
                      >
                        {a.cycle?.status === "open" ? (
                          <>
                            <Edit3 size={13} />
                            修改成本
                          </>
                        ) : a.kind === "unconfigured" ? (
                          "配置"
                        ) : (
                          "新周期"
                        )}
                      </button>
                    )}
                    {a.kind === "paid" && a.cycle?.status === "open" && (
                      <>
                        <button
                          className="text-button"
                          onClick={() => onSettle([a.cycle!.id])}
                        >
                          结算
                        </button>
                        <button
                          className="text-button primary-text"
                          onClick={() => onSettle([a.cycle!.id], true)}
                        >
                          结算并续费
                        </button>
                      </>
                    )}
                  </div>
                )}
                {a.cycle && (
                  <button
                    className="icon-button"
                    aria-label={`${a.name} 周期明细`}
                    onClick={() => onDetail(a.cycle!.id)}
                  >
                    <ChevronRight size={17} />
                  </button>
                )}
              </td>
            </tr>
          ))}
        </tbody>
      </table>
      {!accounts.length && (
        <Empty text={hasActiveVendors ? "暂无独立账号" : "暂无账号成本"} />
      )}
    </div>
  );
});

export const ExpensesList = memo(function ExpensesList({
  items,
  actorId,
  isAdmin,
  onDetail,
  onVoid,
  onEdit,
  onSettle,
}: {
  items: ExpenseCalculation[];
  actorId: number;
  isAdmin: boolean;
  onDetail: (item: ExpenseCalculation) => void;
  onVoid: (item: ExpenseCalculation) => void;
  onEdit: (item: ExpenseCalculation) => void;
  onSettle: (expenseId: number) => void;
}) {
  return (
    <div className="expense-list">
      {items.map((item) => {
        const expense = item.expense;
        const mine = item.allocations.find((a) => a.user_id === actorId);
        return (
          <article className="expense-row" key={expense.id}>
            <div className="expense-main">
              <div className="expense-title">
                <strong>{expense.name}</strong>
                <span>
                  #{expense.id} · {expense.incurred_on}
                </span>
              </div>
              <div className="expense-amount">
                <strong>{money(expense.cost_fen)}</strong>
                <Status value={expense.status || "open"} />
              </div>
            </div>
            <div className="expense-meta">
              <span>
                垫付人 <b>{item.creditor_name || "—"}</b>
              </span>
              <span>
                {isAdmin ? `${item.allocations.length} 人参与` : "我的分摊"}{" "}
                <b>
                  {mine
                    ? `${(Number(mine.share) * 100).toFixed(2)}% · ${money(mine.amount_fen)}`
                    : "—"}
                </b>
              </span>
            </div>
            <div className="expense-actions">
              <button className="text-button" onClick={() => onDetail(item)}>
                明细
              </button>
              {isAdmin && expense.status === "settled" && (
                <button className="text-button" onClick={() => onVoid(item)}>
                  作废重算
                </button>
              )}
              {isAdmin && expense.status === "open" && (
                <>
                  <button className="text-button" onClick={() => onEdit(item)}>
                    <Edit3 size={13} />
                    修改
                  </button>
                  <button
                    className="text-button primary-text"
                    onClick={() => onSettle(expense.id)}
                  >
                    结算
                  </button>
                </>
              )}
            </div>
          </article>
        );
      })}
      {!items.length && <Empty text="暂无固定费用" />}
    </div>
  );
});
