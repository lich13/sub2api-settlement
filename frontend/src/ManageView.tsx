import { date, money } from "./api";
import { Empty, FormError, Status } from "./Forms";
import type { Cycle, DeletionJob } from "./types";

const deletionLabels: Record<string, string> = {
  queued: "等待删除",
  running: "删除中",
  verify: "等待核验",
  failed: "删除未完成",
};

export function ManageCyclesTable({
  cycles,
  onDetail,
  onManageVendor,
  onEditCycle,
  onSettle,
  onVoid,
}: {
  cycles: Cycle[];
  onDetail: (cycleId: number) => void;
  onManageVendor: (vendorId?: number) => void;
  onEditCycle: (cycle: Cycle) => void;
  onSettle: (ids: number[]) => void;
  onVoid: (cycle: Cycle) => void;
}) {
  return (
    <div className="table-shell">
      <table className="history-table">
        <thead>
          <tr>
            <th>账号 / 周期</th>
            <th className="numeric">成本</th>
            <th>垫付人</th>
            <th>状态</th>
            <th className="action-cell">操作</th>
          </tr>
        </thead>
        <tbody>
          {cycles.map((c) => (
            <tr key={c.id}>
              <td data-label="账号">
                <strong>{c.name}</strong>
                <small>
                  {date(c.start)} — {date(c.end)}
                </small>
              </td>
              <td data-label="成本" className="numeric amount">
                <span className="cost-kind">
                  {c.cost_kind === "estimated"
                    ? "历史待确认成本"
                    : "准确成本"}
                </span>
                {c.allocation_pending ? "待账单拆分" : money(c.cost_fen)}
              </td>
              <td data-label="垫付人">{c.creditor_name}</td>
              <td data-label="状态">
                <Status value={c.status} />
                {c.status === "open" && !c.start_confirmed && (
                  <small className="warning-text">起点未确认</small>
                )}
                {!!c.start_locked && (
                  <small className="locked-start">起点已锁定</small>
                )}
                {!!c.drift && (
                  <small className="warning-text">消费记录有差异</small>
                )}
              </td>
              <td className="action-cell">
                <button className="text-button" onClick={() => onDetail(c.id)}>
                  明细
                </button>
                {c.vendor_period_id ? (
                  <button
                    className="text-button"
                    onClick={() => onManageVendor(c.vendor_id ?? undefined)}
                  >
                    整组管理
                  </button>
                ) : c.status === "open" ? (
                  <>
                    <button
                      className="text-button"
                      onClick={() => onEditCycle(c)}
                    >
                      修改
                    </button>
                    <button
                      className="text-button"
                      onClick={() => onSettle([c.id])}
                    >
                      结算
                    </button>
                  </>
                ) : (
                  <button
                    className="text-button danger"
                    onClick={() => onVoid(c)}
                  >
                    作废
                  </button>
                )}
              </td>
            </tr>
          ))}
        </tbody>
      </table>
      {!cycles.length && <Empty text="暂无周期" />}
    </div>
  );
}

export function DeletionsList({
  deletions,
  busy,
  onRetry,
  onCancel,
}: {
  deletions: DeletionJob[];
  busy: boolean;
  onRetry: (job: DeletionJob) => void;
  onCancel: (job: DeletionJob) => void;
}) {
  const active = deletions.filter(
    (j) => !["done", "cancelled"].includes(j.status),
  );
  return (
    <div className="deletion-list">
      {active.map((job) => (
        <div className="deletion-row" key={job.id}>
          <div>
            <strong>{job.account_name}</strong>
            <span>{deletionLabels[job.status] || job.status}</span>
            <FormError error={job.error || ""} />
          </div>
          <span className="muted">{date(job.updated_at)}</span>
          <div className="row-actions">
            {job.status === "failed" && (
              <button
                className="text-button"
                disabled={busy}
                onClick={() => onRetry(job)}
              >
                重试删除
              </button>
            )}
            {["failed", "queued"].includes(job.status) && (
              <button
                className="text-button"
                disabled={busy}
                onClick={() => onCancel(job)}
              >
                取消删除
              </button>
            )}
          </div>
        </div>
      ))}
      {!active.length && <Empty text="暂无待处理删除" />}
    </div>
  );
}
