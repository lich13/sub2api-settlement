import { useState } from "react";
import { ChevronRight } from "lucide-react";
import { date, money } from "./api";
import { DeletionTargets, type DeletionScopeValue } from "./DeletionScope";
import { Allocation, Empty, FormError, StartConfirmation, Status } from "./Forms";
import { Modal } from "./Modal";
import { NettingDetails } from "./Netting";
import type {
  Calculation,
  Cycle,
  ExpenseCalculation,
  NetDetail,
  Order,
} from "./types";

export type InboxItem = {
  id: number;
  title: string;
  order_id: number | null;
  created_at: string;
};

export type DetailState =
  | { netting: NetDetail }
  | {
      snapshot: Calculation | ExpenseCalculation;
      order?: Order;
      drift?: boolean;
    };

export type ConfirmState = {
  text: string;
  action: (memo?: { note?: string; reference?: string }) => Promise<unknown>;
  payment?: boolean;
  deletionScope?: DeletionScopeValue;
};

export function StartConfirmDialog({
  cycle,
  onClose,
  onConfirmed,
}: {
  cycle: Cycle;
  onClose: () => void;
  onConfirmed: () => Promise<void>;
}) {
  const [modalState, setModalState] = useState({ busy: false, dirty: false });
  return (
    <Modal
      title="确认起点"
      busy={modalState.busy}
      dirty={modalState.dirty}
      close={() => !modalState.busy && onClose()}
    >
      <div className="form">
        <p className="candidate-result">当前估算：{date(cycle.start)}</p>
        <StartConfirmation
          calculation={{
            cycle,
            end: "",
            total_consumption: "0",
            record_count: 0,
            allocations: [],
            start_lower: cycle.start_lower,
          }}
          onStateChange={setModalState}
          onConfirmed={onConfirmed}
        />
      </div>
    </Modal>
  );
}

export function DetailDialog({
  detail,
  currentCycle,
  onClose,
}: {
  detail: DetailState;
  currentCycle: (cycle: Cycle) => Cycle;
  onClose: () => void;
}) {
  const isNetting = "netting" in detail;
  const title = isNetting
    ? "对冲明细"
    : detail.order
      ? `订单 #${detail.order.id}`
      : "source_type" in detail.snapshot &&
          detail.snapshot.source_type === "fixed_expense"
        ? "固定费用明细"
        : "周期明细";
  return (
    <Modal title={title} wide close={onClose}>
      <div className="form">
        {isNetting ? (
          <NettingDetails value={detail.netting} />
        ) : (
          <>
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
            <Allocation
              value={
                "cycle" in detail.snapshot &&
                detail.snapshot.display?.settled === false
                  ? {
                      ...detail.snapshot,
                      cycle: currentCycle(detail.snapshot.cycle),
                    }
                  : detail.snapshot
              }
            />
            {"cycle" in detail.snapshot &&
              !!detail.snapshot.cycle.start_locked && (
                <p className="detail-time">
                  付费起点已锁定 · {date(detail.snapshot.cycle.start)}
                </p>
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
          </>
        )}
      </div>
    </Modal>
  );
}

export function InboxDialog({
  items,
  onClose,
  onOpenOrder,
}: {
  items: InboxItem[];
  onClose: () => void;
  onOpenOrder: (orderId: number) => void;
}) {
  return (
    <Modal title="通知" close={onClose}>
      <div className="inbox">
        {items.map((n) =>
          n.order_id !== null ? (
            <button
              key={n.id}
              type="button"
              className="inbox-item inbox-link"
              onClick={() => {
                onClose();
                onOpenOrder(n.order_id!);
              }}
            >
              <div>
                <strong>{n.title}</strong>
                <time>{date(n.created_at)}</time>
              </div>
              <ChevronRight size={16} />
            </button>
          ) : (
            <div key={n.id} className="inbox-item">
              <div>
                <strong>{n.title}</strong>
                <time>{date(n.created_at)}</time>
              </div>
            </div>
          ),
        )}
        {!items.length && <Empty text="暂无通知" />}
      </div>
    </Modal>
  );
}

export function ConfirmActionDialog({
  confirm,
  busy,
  error,
  onClose,
  onSubmit,
}: {
  confirm: ConfirmState;
  busy: boolean;
  error: string;
  onClose: () => void;
  onSubmit: (memo?: { note?: string; reference?: string }) => void;
}) {
  const [paymentNote, setPaymentNote] = useState("");
  const [paymentReference, setPaymentReference] = useState("");
  return (
    <Modal
      title="确认操作"
      busy={busy}
      dirty={!!(paymentNote || paymentReference)}
      close={() => !busy && onClose()}
    >
      <div className="form">
        <p className="confirm-text">{confirm.text}</p>
        {confirm.deletionScope && (
          <DeletionTargets value={confirm.deletionScope} />
        )}
        {confirm.payment && (
          <>
            <label>
              备注（选填）
              <textarea
                maxLength={500}
                value={paymentNote}
                disabled={busy}
                onChange={(e) => setPaymentNote(e.target.value)}
              />
            </label>
            <label>
              转账凭证编号（选填）
              <input
                maxLength={128}
                value={paymentReference}
                disabled={busy}
                onChange={(e) => setPaymentReference(e.target.value)}
              />
            </label>
          </>
        )}
        <FormError error={error} />
        <footer>
          <button className="button" disabled={busy} onClick={onClose}>
            取消
          </button>
          <button
            className="button primary"
            disabled={busy}
            onClick={() =>
              onSubmit(
                confirm.payment
                  ? {
                      ...(paymentNote ? { note: paymentNote } : {}),
                      ...(paymentReference
                        ? { reference: paymentReference }
                        : {}),
                    }
                  : undefined,
              )
            }
          >
            {busy ? "处理中…" : "确认"}
          </button>
        </footer>
      </div>
    </Modal>
  );
}
