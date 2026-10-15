import { useCallback, useEffect, useRef, useState } from "react";
import { api } from "./api";
import type { AccountState } from "./types";

export function useAccountState(userId?: number) {
  const [state, setState] = useState<AccountState | null>(null);
  const generation = useRef(0);
  const latest = useRef<AccountState | null>(null);
  const refresh = useCallback(async () => {
    if (!userId) return;
    const stamp = generation.current;
    try {
      const next = await api<AccountState>("/accounts/state");
      if (stamp !== generation.current || (latest.current && next.revision < latest.current.revision)) return;
      latest.current = next;
      setState(next);
    } catch {
      if (stamp === generation.current) setState(prev => prev ? { ...prev, error: "账号状态核对失败，暂保留最近列表" } : null);
    }
  }, [userId]);
  useEffect(() => {
    ++generation.current; latest.current = null; setState(null);
    void refresh();
    const visible = () => { if (!document.hidden) void refresh(); };
    const timer = setInterval(visible, 5000);
    document.addEventListener("visibilitychange", visible);
    return () => { ++generation.current; clearInterval(timer); document.removeEventListener("visibilitychange", visible); };
  }, [refresh]);
  return { state, refresh };
}
