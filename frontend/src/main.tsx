import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { createRoot } from "react-dom/client";
import ArrowDownLeft from "lucide-react/icons/arrow-down-left";
import Bell from "lucide-react/icons/bell";
import CircleCheck from "lucide-react/icons/circle-check";
import Layers3 from "lucide-react/icons/layers-3";
import Plus from "lucide-react/icons/plus";
import Search from "lucide-react/icons/search";
import Settings2 from "lucide-react/icons/settings-2";
import Wallet from "lucide-react/icons/wallet";
import { api, ApiError, formatVersion, money, setCsrf } from "./api";
import type {
  Account,
  Actor,
  Calculation,
  Catalog,
  Cycle,
  DeletionJob,
  ExpenseCalculation,
  NetBalance,
  NetDetail,
  NetPayment,
  Order,
  Overview,
  Vendor,
} from "./types";
import { useAccountState } from "./useAccountState";
import {
  VendorGroups,
  VendorManager,
  currentPeriod,
  expandVendorSelection,
  visibleVendorGroups,
  type GroupAction,
} from "./Vendors";
import { CycleForm, Empty, ExpenseForm, FormError, UpgradeForm } from "./Forms";
import { SettleForm } from "./SettleForm";
import { Select } from "./Select";
import { NettingList, NettingReceivables } from "./Netting";
import { AuditLog, OperationStatus } from "./Operations";
import type { DeletionScopeValue } from "./DeletionScope";
import { AccountsTable, ExpensesList } from "./AccountsView";
import { DeletionsList, ManageCyclesTable } from "./ManageView";
import {
  ConfirmActionDialog,
  DetailDialog,
  InboxDialog,
  StartConfirmDialog,
  type ConfirmState,
  type DetailState,
  type InboxItem,
} from "./DetailDialogs";
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
  const [inbox, setInbox] = useState<{ items: InboxItem[]; unread: number }>({
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
  const [detail, setDetail] = useState<DetailState | null>(null);
  const [showInbox, setShowInbox] = useState(false);
  const [startCycle, setStartCycle] = useState<Cycle | null>(null);
  const [confirm, setConfirm] = useState<ConfirmState | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");
  const [toast, setToast] = useState("");
  const isAdmin = me?.role === "admin";
  const [deletions, setDeletions] = useState<DeletionJob[]>([]);
  const { state: accountState, refresh: refreshAccountState } = useAccountState(
    me?.id,
  );
  const active = useMemo(
    () => (accountState?.revision ? new Set(accountState.active_ids) : null),
    [accountState?.revision, accountState?.active_ids],
  );
  const latestNames = useMemo(
    () => new Map((accountState?.accounts || []).map((a) => [a.id, a.name])),
    [accountState?.accounts],
  );
  const currentName = useCallback(
    (id: number, fallback: string, revision = "") =>
      accountState && accountState.revision >= revision
        ? (latestNames.get(id) ?? fallback)
        : fallback,
    [accountState, latestNames],
  );
  const currentCycle = useCallback(
    (c: Cycle, revision = "") =>
      c.status === "open"
        ? { ...c, name: currentName(c.account_id, c.name, revision) }
        : c,
    [currentName],
  );
  const overview = useMemo(
    () =>
      overviewSource && {
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
      },
    [overviewSource, active, currentName, currentCycle],
  );
  const catalog = useMemo(
    () =>
      catalogSource && {
        ...catalogSource,
        accounts: catalogSource.accounts
          .filter((a) => !active || active.has(a.id))
          .map((a) => ({
            ...a,
            name: currentName(a.id, a.name, catalogSource.account_revision),
          })),
      },
    [catalogSource, active, currentName],
  );
  const history = useMemo(
    () => ({
      ...historySource,
      cycles: historySource.cycles
        .filter((c) => !active || active.has(c.account_id))
        .map((c) => currentCycle(c, historySource.account_revision)),
    }),
    [historySource, active, currentCycle],
  );
  const vendors = useMemo(
    () =>
      vendorsSource.map((v) => ({
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
      })),
    [vendorsSource, vendorRevision, active, accountState, currentName, currentCycle],
  );
  const refreshSequence = useRef(0);
  const detailSequence = useRef(0);
  function closeDetail() {
    ++detailSequence.current;
    setDetail(null);
  }

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
      setCsrf("");
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
      closeDetail();
      setConfirm(null);
      setShowInbox(false);
      setStartCycle(null);
    };
    window.addEventListener("settlement:expired", clear);
    return () => window.removeEventListener("settlement:expired", clear);
  }, []);
  const refreshing = useRef(false);
  async function refresh(background = false) {
    if (!me) return;
    refreshing.current = true;
    const sequence = ++refreshSequence.current;
    const scope = tab === "bills" ? "mine" : "all";
    const needOverview =
      !overviewSource || tab !== "manage" || manageTab === "cycles";
    const needCatalog =
      !!isAdmin &&
      (!catalogSource ||
        !!form ||
        !!vendorManager ||
        !!settlement ||
        (!background &&
          (tab === "accounts" ||
            (tab === "manage" && manageTab === "cycles"))));
    const needVendors =
      (tab === "accounts" && (!background || costView === "accounts")) ||
      (!!isAdmin &&
        ((tab === "manage" && manageTab === "cycles") ||
          !!vendorManager ||
          !!settlement));
    const needOrders =
      tab === "bills" || (tab === "manage" && manageTab === "orders");
    const needReceivable = tab === "receivable";
    const needExpenses =
      (tab === "accounts" && (!background || costView === "expenses")) ||
      !!form?.expense ||
      !!form?.newExpense ||
      !!settlement?.expenseIds.length;
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
        needOverview ? api<Overview>("/overview") : Promise.resolve(null),
        api<{ items: InboxItem[]; unread: number }>("/inbox"),
        needCatalog ? api<Catalog>("/catalog") : Promise.resolve(null),
        needVendors
          ? api<{ vendors: Vendor[]; account_revision?: string }>("/vendors")
          : Promise.resolve(null),
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
      if (data) setOverview(data);
      setInbox(messages);
      if (accounts) setCatalog(accounts);
      if (supplierData) {
        setVendors(supplierData.vendors);
        setVendorRevision(supplierData.account_revision || "");
      }
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
      if (sequence === refreshSequence.current) setError((e as Error).message);
    } finally {
      if (sequence === refreshSequence.current) refreshing.current = false;
    }
  }
  const latestRefresh = useRef(refresh);
  latestRefresh.current = refresh;
  useEffect(() => {
    if (!me) return;
    if (!(tab === "manage" && manageTab === "audit" && overviewSource)) {
      void latestRefresh.current(false);
    }
    const poll = (background: boolean) => {
      if (!document.hidden && !refreshing.current) {
        void latestRefresh.current(background);
      }
    };
    const onVisible = () => poll(false);
    const timer = setInterval(() => poll(true), 10_000);
    document.addEventListener("visibilitychange", onVisible);
    return () => {
      refreshing.current = false;
      ++refreshSequence.current;
      clearInterval(timer);
      document.removeEventListener("visibilitychange", onVisible);
    };
  }, [me, tab, manageTab, costView]);
  const discoveredAccounts = useRef("");
  useEffect(() => {
    const canRefreshOverview =
      !overviewSource || tab !== "manage" || manageTab === "cycles";
    if (
      !canRefreshOverview ||
      !accountState?.accounts ||
      !overviewSource ||
      accountState.error
    )
      return;
    const known = new Set(overviewSource.accounts.map((a) => a.id));
    const added = accountState.active_ids
      .filter((id) => !known.has(id))
      .sort((a, b) => a - b)
      .join(",");
    if (added && added !== discoveredAccounts.current)
      void latestRefresh.current();
    discoveredAccounts.current = added;
  }, [accountState, overviewSource, tab, manageTab]);
  useEffect(() => {
    if (!toast) return;
    const timer = setTimeout(() => setToast(""), 3500);
    return () => clearTimeout(timer);
  }, [toast]);
  function move(next: string) {
    closeDetail();
    setTab(next);
    setSelected([]);
    setQuery("");
    setFilter("all");
    setError("");
  }
  const allVendorPeriods = useMemo(
    () => vendors.flatMap((v) => v.periods),
    [vendors],
  );
  const openVendorPeriods = useMemo(
    () => allVendorPeriods.filter((p) => p.status === "open"),
    [allVendorPeriods],
  );
  const beginSettlement = useCallback(
    (ids: number[], renew = false) => {
      setSettlement({
        ids: expandVendorSelection(ids, openVendorPeriods),
        expenseIds: [],
        renew,
      });
    },
    [openVendorPeriods],
  );
  const toggle = useCallback((id: number) => {
    setSelected((s) =>
      s.includes(id) ? s.filter((x) => x !== id) : [...s, id],
    );
  }, []);
  const toggleVendorGroup = useCallback((ids: number[]) => {
    const groupSet = new Set(ids);
    setSelected((prev) =>
      prev.some((id) => groupSet.has(id))
        ? prev.filter((id) => !groupSet.has(id))
        : [...new Set([...prev, ...ids])],
    );
  }, []);
  const handleManageVendor = useCallback(
    (id?: number, action?: Exclude<GroupAction, "create">) =>
      setVendorManager({ id, action }),
    [],
  );
  const handleUpgradeAccount = useCallback(
    (upgrade: Account) => setForm({ upgrade }),
    [],
  );
  const handleEditAccountCycle = useCallback(
    (account: Account, cycle?: Cycle) =>
      setForm({ account, ...(cycle ? { cycle } : {}) }),
    [],
  );
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
        closeDetail();
        await refresh();
      }
    } finally {
      setBusy(false);
    }
  }
  const latestRun = useRef(run);
  latestRun.current = run;
  async function saved(message?: string) {
    setError("");
    setForm(null);
    setSettlement(null);
    setSelected([]);
    setToast(message || "已保存");
    await refreshAccountState();
    await refresh();
  }
  const showOrder = useCallback(async (id: number) => {
    const ticket = ++detailSequence.current;
    try {
      const value = await api<{
        snapshot: Calculation | ExpenseCalculation;
        order: Order;
        drift?: boolean;
      }>(`/orders/${id}`);
      if (ticket === detailSequence.current) setDetail(value);
    } catch (e) {
      if (ticket === detailSequence.current) setError((e as Error).message);
    }
  }, []);
  const showCycle = useCallback(async (id: number) => {
    const ticket = ++detailSequence.current;
    try {
      const snapshot = await api<Calculation>(`/cycles/${id}`);
      if (ticket === detailSequence.current) setDetail({ snapshot });
    } catch (e) {
      if (ticket === detailSequence.current) setError((e as Error).message);
    }
  }, []);
  const openConfirm = useCallback((next: ConfirmState) => {
    setError("");
    setConfirm(next);
  }, []);
  const orderAction = useCallback(
    (o: Order) => {
      const group = orders.filter((row) =>
        o.batch_id ? row.batch_id === o.batch_id : row.id === o.id,
      );
      openConfirm({
        text: `标记 ${group.length} 张历史待处理订单已结清 ${money(group.reduce((sum, row) => sum + row.amount_fen, 0))}`,
        action: () =>
          api(`/orders/${o.id}/actions`, "POST", {
            action: "mark_paid",
            version: o.version,
          }),
      });
    },
    [orders, openConfirm],
  );
  const showNetting = useCallback(async (value: NetBalance | NetPayment) => {
    const ticket = ++detailSequence.current;
    try {
      const path =
        "id" in value
          ? `/netting/payments/${value.id}`
          : `/netting/balances/${value.user_a}/${value.user_b}`;
      const netting = await api<NetDetail>(path);
      if (ticket === detailSequence.current) setDetail({ netting });
    } catch (e) {
      if (ticket === detailSequence.current) setError((e as Error).message);
    }
  }, []);
  const payNetting = useCallback(
    (value: NetBalance) => {
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
      openConfirm({
        text,
        payment: true,
        action: (memo) =>
          api("/netting/payments", "POST", { ...payload, ...memo }),
      });
    },
    [openConfirm],
  );
  const retryDeletion = useCallback(async (job: DeletionJob) => {
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
  }, []);
  const cancelDeletion = useCallback((job: DeletionJob) => {
    void latestRun.current(
      () =>
        api(`/account-deletions/${job.id}/cancel`, "POST", {
          version: job.version,
        }),
      "已取消删除任务",
    );
  }, []);
  const showExpenseDetail = useCallback((item: ExpenseCalculation) => {
    ++detailSequence.current;
    setDetail({ snapshot: item });
  }, []);
  const voidExpense = useCallback(
    (item: ExpenseCalculation) =>
      openConfirm({
        text: `作废「${item.expense.name}」本次结算？已报已付或结清的账目无法作废。`,
        action: () =>
          api(`/settlements/${item.expense.settlement_id}/void`, "POST", {}),
      }),
    [openConfirm],
  );
  const editExpense = useCallback(
    (expense: ExpenseCalculation) => setForm({ expense }),
    [],
  );
  const settleExpense = useCallback(
    (expenseId: number) =>
      setSettlement({
        ids: [],
        expenseIds: [expenseId],
        renew: false,
      }),
    [],
  );
  const handleManageCycleEdit = useCallback(
    (cycle: Cycle) => setForm({ cycle }),
    [],
  );
  const voidCycle = useCallback(
    (c: Cycle) =>
      openConfirm({
        text: `作废「${c.name}」本次结算，并恢复为待重算周期`,
        action: () => api(`/settlements/${c.settlement_id}/void`, "POST"),
      }),
    [openConfirm],
  );
  const normalizedQuery = query.trim().toLowerCase();
  const selectedSet = useMemo(() => new Set(selected), [selected]);
  const filteredAccounts = useMemo(
    () =>
      (overview?.accounts || []).filter(
        (a) =>
          !a.deleted &&
          !a.vendor_id &&
          (isAdmin || a.kind === "paid") &&
          (!normalizedQuery ||
            `${a.name} ${a.id}`.toLowerCase().includes(normalizedQuery)) &&
          (filter === "all" || filter === "open"
            ? filter === "all" ||
              (a.kind === "paid" && a.cycle?.status === "open")
            : a.kind === filter),
      ),
    [overview, isAdmin, normalizedQuery, filter],
  );
  const visibleVendors = useMemo(
    () =>
      visibleVendorGroups(vendors, overview?.accounts || [], query, filter),
    [vendors, overview, query, filter],
  );
  const visibleVendorCycleIds = useMemo(
    () =>
      visibleVendors.flatMap(({ period }) =>
        period?.members?.map((c) => c.id) || [],
      ),
    [visibleVendors],
  );
  const selectableCycleIds = useMemo(
    () => [
      ...new Set([
        ...visibleVendorCycleIds,
        ...filteredAccounts.flatMap((a) =>
          a.kind === "paid" && a.cycle?.status === "open" ? [a.cycle.id] : [],
        ),
      ]),
    ],
    [visibleVendorCycleIds, filteredAccounts],
  );
  const allVisibleSelected =
    selectableCycleIds.length > 0 &&
    selectableCycleIds.every((id) => selectedSet.has(id));
  const toggleAllVisible = useCallback(() => {
    const visibleSet = new Set(selectableCycleIds);
    setSelected((prev) =>
      allVisibleSelected
        ? prev.filter((id) => !visibleSet.has(id))
        : [...new Set([...prev, ...selectableCycleIds])],
    );
  }, [allVisibleSelected, selectableCycleIds]);
  const filteredExpenses = useMemo(
    () =>
      expenses.filter(
        (item) =>
          !normalizedQuery ||
          `${item.expense.name} ${item.expense.id} ${item.creditor_name || ""}`
            .toLowerCase()
            .includes(normalizedQuery),
      ),
    [expenses, normalizedQuery],
  );
  const billPage =
    tab === "bills" || (tab === "manage" && manageTab === "orders");
  const receivablePage = tab === "receivable";
  const availableCycles = useMemo(() => {
    const map = new Map<number, Cycle>();
    for (const account of overview?.accounts || []) {
      if (account.cycle) map.set(account.cycle.id, account.cycle);
    }
    for (const cycle of history.cycles) map.set(cycle.id, cycle);
    for (const v of vendors)
      for (const p of v.periods)
        for (const c of p.members || [])
          if (!map.has(c.id) || map.get(c.id)!.version < c.version)
            map.set(c.id, currentCycle(c));
    return map;
  }, [overview, history.cycles, vendors, currentCycle]);
  const settleSelection = useMemo(
    () => expandVendorSelection(settlement?.ids || [], openVendorPeriods),
    [settlement?.ids, openVendorPeriods],
  );
  const settleCycles = useMemo(
    () =>
      settleSelection
        .map((id) => availableCycles.get(id))
        .filter((cycle): cycle is Cycle => !!cycle),
    [settleSelection, availableCycles],
  );
  const hasActiveVendors = useMemo(
    () => vendors.some((v) => !v.archived_at),
    [vendors],
  );
  const topAlerts = [
    ...new Set(
      [
        !confirm ? error : "",
        tab !== "manage" ? overview?.sync_error : "",
        tab !== "manage"
          ? accountState?.error || overview?.account_sync_error
          : "",
      ].filter((msg): msg is string => !!msg),
    ),
  ];

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
          <span className="app-version">{formatVersion(me?.version)}</span>
        </div>
        <div className="header-right">
          {me && (
            <span className="identity">
              <span className="identity-name">{me.name}</span>
              <small>{isAdmin ? "管理员" : "成员"}</small>
            </span>
          )}
          <button
            className="icon-button bell"
            aria-label="通知"
            onClick={() => {
              setShowInbox(true);
              if (inbox.unread > 0) {
                setInbox((prev) => (prev.unread ? { ...prev, unread: 0 } : prev));
                void api("/inbox/read", "POST").catch(() => {
                  void latestRefresh.current(false);
                });
              }
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
        {topAlerts.map((msg) => (
          <FormError key={msg} error={msg} />
        ))}
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
                    closeDetail();
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
                    closeDetail();
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
                      closeDetail();
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
                          onClick={() => beginSettlement(selected, false)}
                        >
                          批量结算 ({selected.length})
                        </button>
                        <button
                          className="button primary"
                          disabled={!catalog}
                          onClick={() => beginSettlement(selected, true)}
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
              <>
                <VendorGroups
                  vendors={vendors}
                  accounts={overview?.accounts || []}
                  groups={visibleVendors}
                  admin={!!isAdmin}
                  totals={overview?.vendor_totals}
                  selected={selected}
                  onToggle={toggleVendorGroup}
                  query={query}
                  filter={filter}
                  onSettle={beginSettlement}
                  onManage={handleManageVendor}
                  onDetail={showCycle}
                />
                <AccountsTable
                  accounts={filteredAccounts}
                  isAdmin={!!isAdmin}
                  syncError={overview.sync_error}
                  hasActiveVendors={hasActiveVendors}
                  selected={selected}
                  selectableCycleIds={selectableCycleIds}
                  allVisibleSelected={allVisibleSelected}
                  onToggleAll={toggleAllVisible}
                  onToggle={toggle}
                  onConfirmStart={setStartCycle}
                  onUpgrade={handleUpgradeAccount}
                  onEditCycle={handleEditAccountCycle}
                  onSettle={beginSettlement}
                  onDetail={showCycle}
                />
              </>
            )}
            {tab === "accounts" && costView === "expenses" && (
              <ExpensesList
                items={filteredExpenses}
                actorId={me.id}
                isAdmin={!!isAdmin}
                onDetail={showExpenseDetail}
                onVoid={voidExpense}
                onEdit={editExpense}
                onSettle={settleExpense}
              />
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
              <ManageCyclesTable
                cycles={history.cycles}
                onDetail={showCycle}
                onManageVendor={handleManageVendor}
                onEditCycle={handleManageCycleEdit}
                onSettle={beginSettlement}
                onVoid={voidCycle}
              />
            )}
            {tab === "manage" && manageTab === "deletions" && (
              <DeletionsList
                deletions={deletions}
                busy={busy}
                onRetry={retryDeletion}
                onCancel={cancelDeletion}
              />
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
            void saved();
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
          key={`${vendorManager.id ?? "all"}:${vendorManager.action ?? "manage"}`}
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
          vendorPeriods={allVendorPeriods}
          catalog={catalog}
          done={saved}
          close={() => setSettlement(null)}
          renewByDefault={settlement.renew}
        />
      )}
      {startCycle && (
        <StartConfirmDialog
          key={startCycle.id}
          cycle={startCycle}
          onClose={() => setStartCycle(null)}
          onConfirmed={async () => {
            setStartCycle(null);
            await refresh();
            setToast("起点已确认");
          }}
        />
      )}
      {detail && (
        <DetailDialog
          detail={detail}
          currentCycle={currentCycle}
          onClose={closeDetail}
        />
      )}
      {showInbox && (
        <InboxDialog
          items={inbox.items}
          onClose={() => setShowInbox(false)}
          onOpenOrder={(orderId) => void showOrder(orderId)}
        />
      )}
      {confirm && (
        <ConfirmActionDialog
          key={confirm.text}
          confirm={confirm}
          busy={busy}
          error={error}
          onClose={() => {
            setConfirm(null);
            setError("");
          }}
          onSubmit={(memo) => void run(() => confirm.action(memo))}
        />
      )}
    </div>
  );
}

const rootElement = document.getElementById("root");
if (rootElement) createRoot(rootElement).render(<App />);
