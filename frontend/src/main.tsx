import { useEffect, useRef, useState } from "react";
import { createRoot } from "react-dom/client";
import {
  ArrowDownLeft,
  Bell,
  ChevronRight,
  CircleAlert,
  CircleCheck,
  Edit3,
  Layers3,
  Plus,
  Search,
  Settings2,
  Wallet,
} from "lucide-react";
import { api, ApiError, setCsrf, money, date } from "./api";
import type {
  Actor,
  Account,
  Catalog,
  Cycle,
  Overview,
  Order,
  ExpenseCalculation,
  NetBalance,
  NetPayment,
  DeletionJob,
  Vendor,
} from "./types";
import { useAccountState } from "./useAccountState";
import {
  VendorGroups,
  VendorManager,
  currentPeriod,
  expandVendorSelection,
  type GroupAction,
} from "./Vendors";
import { Modal } from "./Modal";
import {
  Allocation,
  CycleForm,
  Empty,
  ExpenseForm,
  FormError,
  StartConfirmation,
  Status,
  UpgradeForm,
} from "./Forms";
import { SettleForm } from "./SettleForm";
import { Select } from "./Select";
import { NettingList, NettingReceivables, NettingDetails } from "./Netting";
import { AuditLog, OperationStatus } from "./Operations";
import { DeletionTargets, type DeletionScopeValue } from "./DeletionScope";
import "./style.css";
import settlementIcon from "./assets/settlement.svg?raw";

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
  const [historySource, setHistory] = useState<{
    cycles: Cycle[];
    account_revision?: string;
  }>({
    cycles: [],
  });
  const [inbox, setInbox] = useState<{ items: any[]; unread: number }>({
    items: [],
    unread: 0,
  });
  const [selected, setSelected] = useState<number[]>([]);
  const [form, setForm] = useState<{
    account?: Account;
    cycle?: Cycle;
    expense?: ExpenseCalculation;
    newExpense?: boolean;
    upgrade?: Account;
  } | null>(null);
  const [vendorsSource, setVendors] = useState<Vendor[]>([]);
  const [vendorRevision, setVendorRevision] = useState("");
  const [vendorManager, setVendorManager] = useState<{
    id?: number;
    action?: GroupAction;
  } | null>(null);
  const [settlement, setSettlement] = useState<{
    ids: number[];
    expenseIds: number[];
    renew: boolean;
  } | null>(null);
  const [detail, setDetail] = useState<any>(null);
  const [showInbox, setShowInbox] = useState(false);
  const [startCycle, setStartCycle] = useState<Cycle | null>(null);
  const [startModalState, setStartModalState] = useState({
    busy: false,
    dirty: false,
  });
  const [confirm, setConfirm] = useState<{
    text: string;
    action: (memo?: { note?: string; reference?: string }) => Promise<unknown>;
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
  const { state: accountState, refresh: refreshAccountState } = useAccountState(
    me?.id,
  );
  const active = accountState?.revision
    ? new Set(accountState.active_ids)
    : null;
  const latestNames = new Map(
    (accountState?.accounts || []).map((a) => [a.id, a.name]),
  );
  const currentName = (id: number, fallback: string, revision = "") =>
    accountState && accountState.revision >= revision
      ? (latestNames.get(id) ?? fallback)
      : fallback;
  const currentCycle = (c: Cycle, revision = "") =>
    c.status === "open"
      ? { ...c, name: currentName(c.account_id, c.name, revision) }
      : c;
  const overview = overviewSource && {
    ...overviewSource,
    accounts: overviewSource.accounts
      .filter((a) => !active || active.has(a.id))
      .map((a) => ({
        ...a,
        name: currentName(a.id, a.name, overviewSource.account_revision),
        cycle: a.cycle
          ? currentCycle(a.cycle, overviewSource.account_revision)
          : null,
      })),
  };
  const catalog = catalogSource && {
    ...catalogSource,
    accounts: catalogSource.accounts
      .filter((a) => !active || active.has(a.id))
      .map((a) => ({
        ...a,
        name: currentName(a.id, a.name, catalogSource.account_revision),
      })),
  };
  const history = {
    ...historySource,
    cycles: historySource.cycles
      .filter((c) => !active || active.has(c.account_id))
      .map((c) => currentCycle(c, historySource.account_revision)),
  };
  const vendors = vendorsSource.map((v) => ({
    ...v,
    current_members: v.current_members?.map((a) => ({
      ...a,
      name: currentName(a.account_id, a.name, vendorRevision),
      account_deleted:
        active && accountState && accountState.revision >= vendorRevision
          ? !active.has(a.account_id)
          : a.account_deleted,
    })),
    periods: v.periods.map((p) => ({
      ...p,
      members: p.members?.map((c) => ({
        ...currentCycle(c, vendorRevision),
        current_name: currentName(
          c.account_id,
          c.current_name || c.name,
          vendorRevision,
        ),
        account_deleted:
          active && accountState && accountState.revision >= vendorRevision
            ? !active.has(c.account_id)
            : c.account_deleted,
      })),
    })),
  }));
  const refreshSequence = useRef(0);

  useEffect(() => {
    if (!active) return;
    const known = [
      ...(overviewSource?.accounts.flatMap((a) => (a.cycle ? [a.cycle] : [])) ||
        []),
      ...historySource.cycles,
    ];
    const removed = new Set(
      known
        .filter((c) => !active.has(c.account_id) && !c.vendor_period_id)
        .map((c) => c.id),
    );
    if (
      (tab === "accounts" && costView === "accounts") ||
      (tab === "manage" && manageTab === "cycles")
    ) {
      setSelected((prev) =>
        prev.some((id) => removed.has(id))
          ? prev.filter((id) => !removed.has(id))
          : prev,
      );
    }
    if (settlement?.ids.some((id) => removed.has(id))) {
      setSettlement(null);
      setError("账号已删除，结算窗口已关闭");
    }
    const editedAccount =
      form?.account?.id ?? form?.upgrade?.id ?? form?.cycle?.account_id;
    if (editedAccount && !active.has(editedAccount)) {
      setForm(null);
      setError("账号已删除，请选择其他账号");
    }
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
      setVendors([]);
      setVendorManager(null);
      setCatalog(null);
      setHistory({ cycles: [] });
      setDeletions([]);
      ++refreshSequence.current;
      setOrders([]);
      setBalances([]);
      setPayments([]);
      setExpenses([]);
      setInbox({ items: [], unread: 0 });
      setForm(null);
      setSettlement(null);
      setDetail(null);
      setConfirm(null);
      setShowInbox(false);
      setStartCycle(null);
    };
    window.addEventListener("settlement:expired", clear);
    return () => window.removeEventListener("settlement:expired", clear);
  }, []);
  const refreshing = useRef(false);
  async function refresh() {
    if (!me) return;
    refreshing.current = true;
    const sequence = ++refreshSequence.current;
    const scope = tab === "bills" ? "mine" : "all";
    const needOrders =
      tab === "bills" || (tab === "manage" && manageTab === "orders");
    const needReceivable = tab === "receivable";
    const needExpenses = tab === "accounts";
    const needHistory = isAdmin && tab === "manage" && manageTab === "cycles";
    const needDeletions =
      isAdmin && tab === "manage" && manageTab === "deletions";
    try {
      const [
        data,
        messages,
        accounts,
        supplierData,
        receivableNet,
        orderBundle,
        expenseResult,
        past,
        jobs,
      ] = await Promise.all([
        api<Overview>("/overview"),
        api<{ items: any[]; unread: number }>("/inbox"),
        isAdmin ? api<Catalog>("/catalog") : Promise.resolve(null),
        api<{ vendors: Vendor[]; account_revision?: string }>("/vendors"),
        needReceivable
          ? api<{ balances: NetBalance[] }>(
              "/netting/balances?scope=receivable",
            )
          : Promise.resolve(null),
        needOrders
          ? Promise.all([
              api<{ orders: Order[] }>("/orders?scope=" + scope),
              api<{ balances: NetBalance[] }>(
                "/netting/balances?scope=" + scope,
              ),
              api<{ payments: NetPayment[] }>(
                "/netting/payments?scope=" + scope,
              ),
            ])
          : Promise.resolve(null),
        needExpenses
          ? api<{ expenses: ExpenseCalculation[] }>("/expenses")
          : Promise.resolve(null),
        needHistory
          ? api<{ cycles: Cycle[]; account_revision?: string }>("/history")
          : Promise.resolve(null),
        needDeletions
          ? api<{ items: DeletionJob[] }>("/account-deletions")
          : Promise.resolve(null),
      ]);
      if (sequence !== refreshSequence.current) return;
      setOverview(data);
      setInbox(messages);
      setCatalog(accounts);
      setVendors(supplierData.vendors || []);
      setVendorRevision(supplierData.account_revision || "");
      if (receivableNet) setBalances(receivableNet.balances);
      if (orderBundle) {
        const [raw, net, paid] = orderBundle;
        setOrders(raw.orders);
        setBalances(net.balances);
        setPayments(paid.payments);
      }
      if (expenseResult) setExpenses(expenseResult.expenses);
      if (past) setHistory(past);
      if (jobs) setDeletions(jobs.items);
    } catch (e) {
      setError((e as Error).message);
    } finally {
      if (sequence === refreshSequence.current) refreshing.current = false;
    }
  }
  useEffect(() => {
    if (!me) return;
    if (!(tab === "manage" && manageTab === "audit" && overviewSource)) {
      void refresh();
    }
    const poll = () => {
      if (!document.hidden && !refreshing.current) {
        void refresh();
      }
    };
    const timer = setInterval(poll, 10_000);
    document.addEventListener("visibilitychange", poll);
    return () => {
      refreshing.current = false;
      ++refreshSequence.current;
      clearInterval(timer);
      document.removeEventListener("visibilitychange", poll);
    };
  }, [me, tab, manageTab]);
  const discoveredAccounts = useRef("");
  useEffect(() => {
    if (!accountState?.accounts || !overviewSource || accountState.error)
      return;
    const known = new Set(overviewSource.accounts.map((a) => a.id));
    const added = accountState.active_ids
      .filter((id) => !known.has(id))
      .sort((a, b) => a - b)
      .join(",");
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
  function beginSettlement(ids: number[], renew = false) {
    setSettlement({
      ids: expandVendorSelection(
        ids,
        vendors.flatMap((v) => v.periods).filter((p) => p.status === "open"),
      ),
      expenseIds: [],
      renew,
    });
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
        setConfirm(null);
        setDetail(null);
        await refresh();
      }
    } finally {
      setBusy(false);
    }
  }
  async function saved(message?: string) {
    setError("");
    setForm(null);
    setSettlement(null);
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
    const group = orders.filter((row) =>
      o.batch_id ? row.batch_id === o.batch_id : row.id === o.id,
    );
    setConfirm({
      text: `标记 ${group.length} 张历史待处理订单已结清 ${money(group.reduce((sum, row) => sum + row.amount_fen, 0))}`,
      action: () =>
        api(`/orders/${o.id}/actions`, "POST", {
          action: "mark_paid",
          version: o.version,
        }),
    });
  }
  async function showNetting(value: NetBalance | NetPayment) {
    try {
      const path =
        "id" in value
          ? `/netting/payments/${value.id}`
          : `/netting/balances/${value.user_a}/${value.user_b}`;
      setDetail({ netting: await api(path) });
    } catch (e) {
      setError((e as Error).message);
    }
  }
  function payNetting(value: NetBalance) {
    const payerName =
      value.payer_id === value.user_a ? value.name_a : value.name_b;
    const creditorName =
      value.creditor_id === value.user_a ? value.name_a : value.name_b;
    const text = value.amount_fen
      ? `${payerName} 向 ${creditorName} 转账 ${money(value.amount_fen)}，结清双方 ${value.order_count} 张订单`
      : `确认 ${value.name_a} 与 ${value.name_b} 的 ${value.order_count} 张订单相互抵消，无需转账`;
    const payload = {
      user_a: value.user_a,
      user_b: value.user_b,
      digest: value.digest,
    };
    setPaymentNote("");
    setPaymentReference("");
    setConfirm({
      text,
      payment: true,
      action: (memo) => api("/netting/payments", "POST", { ...payload, ...memo }),
    });
  }
  async function retryDeletion(job: DeletionJob) {
    setBusy(true);
    setError("");
    try {
      const scope = await api<DeletionScopeValue>(
        `/accounts/${job.account_id}/deletion-scope`,
      );
      setConfirm({
        text: `重试删除以下 ${scope.targets.length} 个账号`,
        deletionScope: scope,
        action: () =>
          api(`/account-deletions/${job.id}/retry`, "POST", {
            version: job.version,
            scope_digest: scope.digest,
          }),
      });
    } catch (e) {
      setError((e as Error).message);
    } finally {
      setBusy(false);
    }
  }
  const filteredAccounts = (overview?.accounts || []).filter(
    (a) =>
      !a.deleted &&
      !a.vendor_id &&
      (isAdmin || a.kind === "paid") &&
      (!query ||
        `${a.name} ${a.id}`.toLowerCase().includes(query.toLowerCase())) &&
      (filter === "all" || filter === "open"
        ? filter === "all" || (a.kind === "paid" && a.cycle?.status === "open")
        : a.kind === filter),
  );
  const visibleVendorCycleIds = vendors
    .filter(
      (v) =>
        !v.archived_at &&
        (filter === "all" ||
          filter === "paid" ||
          (filter === "open" && currentPeriod(v))),
    )
    .flatMap((v) => {
      const p = currentPeriod(v);
      if (!p) return [];
      const members = (
        v.current_members ||
        p.members?.map((c) => ({
          account_id: c.account_id,
          name: c.current_name || c.name,
          account_deleted: c.account_deleted,
        })) ||
        []
      ).filter((m) => !m.account_deleted);
      if (
        query &&
        !`${v.name} ${members.map((m) => overview?.accounts.find((a) => a.id === m.account_id)?.name || m.name).join(" ")}`
          .toLowerCase()
          .includes(query.toLowerCase())
      )
        return [];
      return p.members?.map((c) => c.id) || [];
    });
  const selectableCycleIds = [
    ...new Set([
      ...visibleVendorCycleIds,
      ...filteredAccounts.flatMap((a) =>
        a.kind === "paid" && a.cycle?.status === "open" ? [a.cycle.id] : [],
      ),
    ]),
  ];
  const allVisibleSelected =
    selectableCycleIds.length > 0 &&
    selectableCycleIds.every((id) => selected.includes(id));
  const filteredExpenses = expenses.filter(
    (item) =>
      !query ||
      `${item.expense.name} ${item.expense.id} ${item.creditor_name || ""}`
        .toLowerCase()
        .includes(query.toLowerCase()),
  );
  const billPage =
    tab === "bills" || (tab === "manage" && manageTab === "orders");
  const receivablePage = tab === "receivable";
  const availableCycles = new Map<number, Cycle>();
  for (const account of overview?.accounts || []) {
    if (account.cycle) availableCycles.set(account.cycle.id, account.cycle);
  }
  for (const cycle of history.cycles) availableCycles.set(cycle.id, cycle);
  for (const v of vendors)
    for (const p of v.periods)
      for (const c of p.members || [])
        if (
          !availableCycles.has(c.id) ||
          availableCycles.get(c.id)!.version < c.version
        )
          availableCycles.set(c.id, currentCycle(c));
  const settleSelection = expandVendorSelection(
    settlement?.ids || [],
    vendors.flatMap((v) => v.periods).filter((p) => p.status === "open"),
  );
  const settleCycles = settleSelection
    .map((id) => availableCycles.get(id))
    .filter((cycle): cycle is Cycle => !!cycle);

  return (
    <div className="app">
      <header className="topbar">
        <div className="brand">
          <div
            className="brand-icon"
            aria-hidden="true"
            dangerouslySetInnerHTML={{ __html: settlementIcon }}
          />
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
        {(accountState?.error || overview?.account_sync_error) && (
          <FormError
            error={accountState?.error || overview?.account_sync_error || ""}
          />
        )}
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
                  {overview.vendor_totals?.some((p) =>
                    p.members.some((m) => m.allocation_pending),
                  ) && <small>待账单拆分</small>}
                </div>
                <div>
                  <span>待收款</span>
                  <strong>{money(overview.receivable_fen)}</strong>
                </div>
              </section>
            )}
            {tab === "accounts" && (
              <div
                className="segments cost-tabs"
                role="tablist"
                aria-label="成本类型"
              >
                <button
                  className={costView === "accounts" ? "active" : ""}
                  onClick={() => {
                    setCostView("accounts");
                    setQuery("");
                    setSelected([]);
                  }}
                  role="tab"
                  aria-selected={costView === "accounts"}
                >
                  账号费用
                </button>
                <button
                  className={costView === "expenses" ? "active" : ""}
                  onClick={() => {
                    setCostView("expenses");
                    setQuery("");
                    setSelected([]);
                  }}
                  role="tab"
                  aria-selected={costView === "expenses"}
                >
                  固定费用
                </button>
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
            {(tab === "accounts" || billPage) && (
              <div className="toolbar">
                <div className="search">
                  <Search size={16} />
                  <input
                    placeholder={
                      tab === "accounts"
                        ? costView === "expenses"
                          ? "搜索固定费用"
                          : "搜索账号"
                        : "搜索订单、账号或用户"
                    }
                    aria-label="搜索"
                    value={query}
                    onChange={(e) => setQuery(e.target.value)}
                  />
                </div>
                {(tab !== "accounts" || costView === "accounts") && (
                  <Select
                    className="filter"
                    aria-label="状态筛选"
                    value={filter}
                    onChange={(e) => {
                      setFilter(e.target.value);
                      if (tab === "accounts") {
                        const openIds = new Set([
                          ...(overview?.accounts.flatMap((a) =>
                            a.cycle?.status === "open" ? [a.cycle.id] : [],
                          ) || []),
                          ...vendors.flatMap(
                            (v) =>
                              currentPeriod(v)?.members?.map((c) => c.id) || [],
                          ),
                        ]);
                        setSelected((prev) =>
                          prev.filter((id) => openIds.has(id)),
                        );
                      } else {
                        setSelected([]);
                      }
                    }}
                  >
                    {(tab === "accounts"
                      ? [
                          ["all", "全部账号"],
                          ["open", "进行中"],
                          ...(isAdmin
                            ? [
                                ["unconfigured", "待配置"],
                                ["free", "免费资源"],
                              ]
                            : []),
                        ]
                      : [
                          ["all", "全部状态"],
                          ["pending", "待付款"],
                          ["submitted", "待管理员处理"],
                          ["paid", "已结清"],
                          ["void", "已作废"],
                        ]
                    ).map(([v, label]) => (
                      <option value={v} key={v}>
                        {label}
                      </option>
                    ))}
                  </Select>
                )}
                <div className="toolbar-actions">
                  {tab === "accounts" &&
                    costView === "accounts" &&
                    isAdmin &&
                    selectableCycleIds.length > 0 &&
                    !allVisibleSelected && (
                      <button
                        type="button"
                        className="text-button"
                        onClick={() =>
                          setSelected((prev) => [
                            ...new Set([...prev, ...selectableCycleIds]),
                          ])
                        }
                      >
                        全选
                      </button>
                    )}
                  {tab === "accounts" &&
                    costView === "accounts" &&
                    isAdmin && (
                      <button
                        className="button"
                        disabled={!catalog}
                        onClick={() => setVendorManager({ action: "create" })}
                      >
                        新建账号组
                      </button>
                    )}
                  {tab === "accounts" &&
                    costView === "accounts" &&
                    isAdmin &&
                    selected.length > 0 && (
                      <>
                        <button
                          className="button"
                          disabled={!catalog}
                          onClick={() => {
                            beginSettlement(selected, false);
                          }}
                        >
                          批量结算 ({selected.length})
                        </button>
                        <button
                          className="button primary"
                          disabled={!catalog}
                          onClick={() => {
                            beginSettlement(selected, true);
                          }}
                        >
                          批量结算并续费 ({selected.length})
                        </button>
                        <button
                          type="button"
                          className="text-button"
                          onClick={() => setSelected([])}
                        >
                          清空
                        </button>
                      </>
                    )}
                  {tab === "accounts" &&
                    costView === "expenses" &&
                    isAdmin && (
                      <button
                        className="button primary"
                        disabled={!catalog}
                        onClick={() => setForm({ newExpense: true })}
                      >
                        <Plus size={16} />
                        新增固定费用
                      </button>
                    )}
                </div>
              </div>
            )}
            {tab === "accounts" && costView === "accounts" && (
              <VendorGroups
                vendors={vendors}
                accounts={overview?.accounts || []}
                admin={!!isAdmin}
                totals={overview?.vendor_totals}
                selected={selected}
                onToggle={(ids) =>
                  setSelected((prev) =>
                    ids.some((id) => prev.includes(id))
                      ? prev.filter((id) => !ids.includes(id))
                      : [...new Set([...prev, ...ids])],
                  )
                }
                query={query}
                filter={filter}
                onSettle={beginSettlement}
                onManage={(id, action) => setVendorManager({ id, action })}
                onDetail={showCycle}
              />
            )}
            {tab === "accounts" && costView === "accounts" && (
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
                            onChange={() =>
                              setSelected((prev) =>
                                allVisibleSelected
                                  ? prev.filter(
                                      (id) => !selectableCycleIds.includes(id),
                                    )
                                  : [
                                      ...new Set([
                                        ...prev,
                                        ...selectableCycleIds,
                                      ]),
                                    ],
                              )
                            }
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
                    {filteredAccounts.map((a) => (
                      <tr
                        key={a.id}
                        className={
                          a.kind !== "paid" ? "compact-account" : undefined
                        }
                      >
                        {isAdmin && (
                          <td className="check-cell">
                            {a.kind === "paid" && a.cycle?.status === "open" && (
                              <input
                                aria-label={`选择 ${a.name}`}
                                type="checkbox"
                                checked={
                                  !!a.cycle && selected.includes(a.cycle.id)
                                }
                                onChange={() => a.cycle && toggle(a.cycle.id)}
                              />
                            )}
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
                                  : a.kind === "paid" && a.cycle
                                    ? `${!a.start_confirmed && a.start_mode === "date_estimate" ? "约 " + date(a.start_lower) + " — " + date(a.start_upper) : !a.start_confirmed && a.start_mode === "minute_estimate" ? "约 " + date(a.start_lower, "minute") : date(a.cycle.start)}${a.cycle.end ? " — " + date(a.cycle.end) : " 起"}`
                                    : a.platform}
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
                              {a.allocation_pending
                                ? "待账单拆分"
                                : money(a.cycle.cost_fen)}
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
                              {a.cycle?.status === "open" &&
                                !a.start_confirmed && (
                                  <button
                                    className="text-button"
                                    onClick={() => setStartCycle(a.cycle)}
                                  >
                                    确认起点
                                  </button>
                                )}
                              {a.kind === "free" ? (
                                <button
                                  className="text-button primary-text"
                                  onClick={() => setForm({ upgrade: a })}
                                >
                                  升级付费
                                </button>
                              ) : (
                                <button
                                  className="text-button"
                                  onClick={() =>
                                    setForm({
                                      account: a,
                                      ...(a.cycle?.status === "open"
                                        ? { cycle: a.cycle }
                                        : {}),
                                    })
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
                              {a.kind === "paid" &&
                                a.cycle?.status === "open" && (
                                  <>
                                    <button
                                      className="text-button"
                                      onClick={() => {
                                        beginSettlement([a.cycle!.id]);
                                      }}
                                    >
                                      结算
                                    </button>
                                    <button
                                      className="text-button primary-text"
                                      onClick={() => {
                                        beginSettlement([a.cycle!.id], true);
                                      }}
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
                {!filteredAccounts.length && (
                  <Empty
                    text={
                      vendors.some((v) => !v.archived_at)
                        ? "暂无独立账号"
                        : "暂无账号成本"
                    }
                  />
                )}
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
                          {isAdmin
                            ? `${item.allocations.length} 人参与`
                            : "我的分摊"}{" "}
                          <b>
                            {mine
                              ? `${(Number(mine.share) * 100).toFixed(2)}% · ${money(mine.amount_fen)}`
                              : "—"}
                          </b>
                        </span>
                      </div>
                      <div className="expense-actions">
                        <button
                          className="text-button"
                          onClick={() => setDetail({ snapshot: item })}
                        >
                          明细
                        </button>
                        {isAdmin && expense.status === "settled" && (
                          <button
                            className="text-button"
                            onClick={() =>
                              setConfirm({
                                text: `作废「${expense.name}」本次结算？已报已付或结清的账目无法作废。`,
                                action: () =>
                                  api(
                                    `/settlements/${expense.settlement_id}/void`,
                                    "POST",
                                    {},
                                  ),
                              })
                            }
                          >
                            作废重算
                          </button>
                        )}
                        {isAdmin && expense.status === "open" && (
                          <>
                            <button
                              className="text-button"
                              onClick={() => setForm({ expense: item })}
                            >
                              <Edit3 size={13} />
                              修改
                            </button>
                            <button
                              className="text-button primary-text"
                              onClick={() =>
                                setSettlement({
                                  ids: [],
                                  expenseIds: [expense.id],
                                  renew: false,
                                })
                              }
                            >
                              结算
                            </button>
                          </>
                        )}
                      </div>
                    </article>
                  );
                })}
                {!filteredExpenses.length && <Empty text="暂无固定费用" />}
              </div>
            )}
            {receivablePage && (
              <NettingReceivables
                balances={balances.filter(
                  (b) => b.creditor_id === me.id && b.amount_fen > 0,
                )}
              />
            )}
            {billPage && (
              <NettingList
                me={me}
                balances={balances}
                payments={payments}
                orders={orders}
                adminView={tab === "manage"}
                filter={filter}
                query={query}
                busy={busy}
                onPay={payNetting}
                onDetail={showNetting}
                onOrder={showOrder}
                onLegacyPaid={orderAction}
              />
            )}
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
                          <span className="cost-kind">
                            {c.cost_kind === "estimated"
                              ? "历史待确认成本"
                              : "准确成本"}
                          </span>
                          {c.allocation_pending
                            ? "待账单拆分"
                            : money(c.cost_fen)}
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
                          {c.vendor_period_id ? (
                            <button
                              className="text-button"
                              onClick={() =>
                                setVendorManager({ id: c.vendor_id ?? undefined })
                              }
                            >
                              整组管理
                            </button>
                          ) : c.status === "open" ? (
                            <>
                              <button
                                className="text-button"
                                onClick={() => setForm({ cycle: c })}
                              >
                                修改
                              </button>
                              <button
                                className="text-button"
                                onClick={() => beginSettlement([c.id])}
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
            {tab === "manage" && manageTab === "deletions" && (
              <div className="deletion-list">
                {deletions
                  .filter((j) => !["done", "cancelled"].includes(j.status))
                  .map((job) => (
                    <div className="deletion-row" key={job.id}>
                      <div>
                        <strong>{job.account_name}</strong>
                        <span>
                          {(
                            {
                              queued: "等待删除",
                              running: "删除中",
                              verify: "等待核验",
                              failed: "删除未完成",
                            } as Record<string, string>
                          )[job.status] || job.status}
                        </span>
                        {job.error && <p className="form-error">{job.error}</p>}
                      </div>
                      <span className="muted">{date(job.updated_at)}</span>
                      <div className="row-actions">
                        {job.status === "failed" && (
                          <button
                            className="text-button"
                            disabled={busy}
                            onClick={() => retryDeletion(job)}
                          >
                            重试删除
                          </button>
                        )}
                        {["failed", "queued"].includes(job.status) && (
                          <button
                            className="text-button"
                            disabled={busy}
                            onClick={() =>
                              run(
                                () =>
                                  api(
                                    `/account-deletions/${job.id}/cancel`,
                                    "POST",
                                    { version: job.version },
                                  ),
                                "已取消删除任务",
                              )
                            }
                          >
                            取消删除
                          </button>
                        )}
                      </div>
                    </div>
                  ))}
                {!deletions.some(
                  (j) => !["done", "cancelled"].includes(j.status),
                ) && <Empty text="暂无待处理删除" />}
              </div>
            )}
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
          done={() => {
            setFilter("all");
            saved();
          }}
          close={() => setForm(null)}
        />
      )}
      {form &&
        !form.expense &&
        !form.newExpense &&
        !form.upgrade &&
        catalog && (
          <CycleForm
            {...form}
            catalog={catalog}
            done={saved}
            close={() => setForm(null)}
          />
        )}
      {vendorManager && catalog && (
        <VendorManager
          initialVendorId={vendorManager.id}
          initialAction={vendorManager.action}
          vendors={vendors}
          accounts={overview?.accounts || []}
          catalog={catalog}
          close={() => setVendorManager(null)}
          refresh={refresh}
        />
      )}
      {settlement && catalog && (
        <SettleForm
          ids={settleSelection}
          expenseIds={settlement.expenseIds}
          expenseRevision={
            settlement.expenseIds.length
              ? JSON.stringify(
                  expenses
                    .filter((e) => settlement.expenseIds.includes(e.expense.id))
                    .map((e) => [e.expense.id, e.expense.version]),
                )
              : ""
          }
          cycles={settleCycles}
          vendorPeriods={vendors.flatMap((v) => v.periods)}
          catalog={catalog}
          done={saved}
          close={() => setSettlement(null)}
          renewByDefault={settlement.renew}
        />
      )}
      {startCycle && (
        <Modal
          title="确认起点"
          busy={startModalState.busy}
          dirty={startModalState.dirty}
          close={() => !startModalState.busy && setStartCycle(null)}
        >
          <div className="form">
            <p className="candidate-result">
              当前估算：{date(startCycle.start)}
            </p>
            <StartConfirmation
              calculation={{
                cycle: startCycle,
                end: "",
                total_consumption: "0",
                record_count: 0,
                allocations: [],
                start_lower: startCycle.start_lower,
              }}
              onStateChange={setStartModalState}
              onConfirmed={async () => {
                setStartCycle(null);
                await refresh();
                setToast("起点已确认");
              }}
            />
          </div>
        </Modal>
      )}
      {detail && (
        <Modal
          title={
            detail.netting
              ? "对冲明细"
              : detail.order
                ? `订单 #${detail.order.id}`
                : detail.snapshot.source_type === "fixed_expense"
                  ? "固定费用明细"
                  : "周期明细"
          }
          wide
          close={() => setDetail(null)}
        >
          <div className="form">
            {detail.netting ? (
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
                    detail.snapshot.display?.settled === false &&
                    detail.snapshot.cycle
                      ? {
                          ...detail.snapshot,
                          cycle: currentCycle(detail.snapshot.cycle),
                        }
                      : detail.snapshot
                  }
                />
                {!!detail.snapshot.cycle?.start_locked && (
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
        <Modal
          title="确认操作"
          busy={busy}
          dirty={!!(paymentNote || paymentReference)}
          close={() => !busy && setConfirm(null)}
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
                onClick={() =>
                  run(() =>
                    confirm.action(
                      confirm.payment
                        ? {
                            ...(paymentNote ? { note: paymentNote } : {}),
                            ...(paymentReference
                              ? { reference: paymentReference }
                              : {}),
                          }
                        : undefined,
                    ),
                  )
                }
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
