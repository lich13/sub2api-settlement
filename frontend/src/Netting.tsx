import { money, date } from "./api";
import type { Actor, NetBalance, NetDetail, NetPayment, Order } from "./types";

const person = (value: NetBalance, id: number | null) => id === value.user_a ? value.name_a : value.name_b;
const pairKey = (value: NetBalance) => `${value.user_a}:${value.user_b}`;
const empty = <div className="empty">暂无账单</div>;

export function NettingReceivables({ balances }: { balances: NetBalance[] }) {
  const items = balances.filter(b => b.amount_fen > 0 && b.creditor_id !== null);
  return <section className="receivable-panel" aria-label="待收订单">
    <div className="receivable-summary"><span>待收合计</span><strong>{money(items.reduce((s, b) => s + b.amount_fen, 0))}</strong></div>
    <div className="receivable-list">{items.map(b => <article className="receivable-row" key={pairKey(b)}>
      <div className="receivable-source"><div><strong>{person(b, b.payer_id)}</strong><span>{b.order_count} 张订单</span></div></div>
      <div className="receivable-status"><span className="status pending">待付款</span></div>
      <strong className="receivable-amount">{money(b.amount_fen)}</strong>
    </article>)}{!items.length && <div className="empty">暂无待收订单</div>}</div>
  </section>;
}

export function NettingDetails({ value }: { value: NetDetail }) {
  return <section className="netting-detail">
    <dl className="netting-totals">
      <div><dt>{value.name_a} 应付 {value.name_b}</dt><dd>{money(value.a_to_b_fen)}</dd></div>
      <div><dt>{value.name_b} 应付 {value.name_a}</dt><dd>{money(value.b_to_a_fen)}</dd></div>
      <div><dt>相互抵扣</dt><dd>{money(value.offset_fen)}</dd></div>
      <div className="netting-final"><dt>{value.amount_fen ? `${person(value, value.payer_id)} → ${person(value, value.creditor_id)}` : "无需付款"}</dt><dd>{money(value.amount_fen)}</dd></div>
    </dl>
    {[value.user_a, value.user_b].map(uid => <div key={uid} className="netting-direction">
      <h3>{person(value, uid)} 的原始订单</h3>
      {value.orders.filter(o => o.payer_id === uid).map(o => <div className="netting-order" key={o.id}>
        <div><strong>{o.account_name}</strong><span>订单 #{o.id}{o.batch_id ? ` · 合并单 #${o.batch_id}` : ""}</span></div>
        <b>{money(o.amount_fen)}</b>
      </div>)}
      {!value.orders.some(o => o.payer_id === uid) && <p className="detail-time">无待抵扣订单</p>}
    </div>)}
    {value.created_at && <p className="detail-time">结清时间 {date(value.created_at)}</p>}
  </section>;
}

export function NettingList({ me, balances, payments, orders, adminView, filter, query, busy, onPay, onDetail, onOrder, onLegacyPaid }: {
  me: Actor; balances: NetBalance[]; payments: NetPayment[]; orders: Order[];
  adminView: boolean; filter: string; query: string; busy: boolean;
  onPay: (value: NetBalance) => void; onDetail: (value: NetBalance | NetPayment) => void;
  onOrder: (id: number) => void; onLegacyPaid: (order: Order) => void;
}) {
  const matches = (text: string) => text.toLowerCase().includes(query.toLowerCase());
  const allPairs = adminView && me.role === "admin";
  const participant = (b: NetBalance) => b.user_a === me.id || b.user_b === me.id;
  const pending = (filter === "all" || filter === "pending") ? balances.filter(b =>
    (allPairs || (participant(b) && (b.payer_id === me.id || b.amount_fen === 0))) && matches(`${b.name_a} ${b.name_b}`)) : [];
  const history = (filter === "all" || filter === "paid") ? payments.filter(b => (allPairs || participant(b)) && matches(`${b.name_a} ${b.name_b} ${b.id}`)) : [];
  const legacy = orders.filter(o => (allPairs || o.payer_id === me.id) && !o.netting_payment_id && o.status !== "pending" &&
    (filter === "all" || filter === o.status) && matches(`${o.account_name} ${o.payer_name} ${o.creditor_name} ${o.id}`));
  const raw = adminView ? orders.filter(o => (filter === "all" || filter === o.status) && matches(`${o.account_name} ${o.payer_name} ${o.creditor_name} ${o.id}`)) : [];
  return <div className="bill-list">
    {pending.map(b => <article className="bill netting-bill" key={pairKey(b)}>
      <div className="bill-main">
        <div className="bill-title"><strong>{adminView ? `${b.name_a} 与 ${b.name_b}` : person(b, me.id === b.user_a ? b.user_b : b.user_a)}</strong><span>{b.order_count} 张订单{b.offset_fen > 0 ? ` · 已抵扣 ${money(b.offset_fen)}` : ""}</span></div>
        <div className="bill-amount"><strong>{b.amount_fen ? money(b.amount_fen) : "无需付款"}</strong><span className="status pending">{b.amount_fen ? "净应付" : "待对冲"}</span></div>
      </div>
      <div className="bill-actions">
        <button className="text-button" onClick={() => onDetail(b)}>明细</button>
        {(me.role === "admin" || b.payer_id === me.id || !b.amount_fen) && <button className="button primary" disabled={busy} onClick={() => onPay(b)}>{!b.amount_fen ? "确认对冲" : adminView && me.id !== b.payer_id ? "标记已结清" : "确认转账"}</button>}
      </div>
    </article>)}
    {history.map(b => <article className="bill netting-bill" key={`payment:${b.id}`}>
      <div className="bill-main"><div className="bill-title"><strong>{b.amount_fen ? `${person(b, b.payer_id)} → ${person(b, b.creditor_id)}` : `${b.name_a} 与 ${b.name_b}`}</strong><span>{b.amount_fen ? "转账结清" : "对冲结清"} · {date(b.created_at)}</span></div><div className="bill-amount"><strong>{b.amount_fen ? money(b.amount_fen) : "无需付款"}</strong><span className="status paid">已结清</span></div></div>
      <div className="bill-actions"><button className="text-button" onClick={() => onDetail(b)}>明细 · {b.order_count} 张订单</button></div>
    </article>)}
    {legacy.map(o => <article className="bill" key={`legacy:${o.id}`}>
      <div className="bill-main"><div className="bill-title"><strong>{o.account_name}</strong><span>{o.payer_name} → {o.creditor_name}</span></div><div className="bill-amount"><strong>{money(o.amount_fen)}</strong><span className={`status ${o.status}`}>{o.status === "paid" ? "已结清" : o.status === "void" ? "已作废" : "待管理员处理"}</span></div></div>
      <div className="bill-actions"><button className="text-button" onClick={() => onOrder(o.id)}>明细</button>{adminView && o.status === "submitted" && <button className="button primary" disabled={busy} onClick={() => onLegacyPaid(o)}>标记已结清</button>}</div>
    </article>)}
    {!pending.length && !history.length && !legacy.length && empty}
    {adminView && <details className="raw-orders"><summary>原始订单 · {raw.length} 张</summary>{raw.map(o => <button className="netting-order" key={o.id} onClick={() => onOrder(o.id)}><div><strong>{o.account_name}</strong><span>#{o.id} · {o.payer_name} → {o.creditor_name}</span></div><b>{money(o.amount_fen)}</b></button>)}</details>}
  </div>;
}
