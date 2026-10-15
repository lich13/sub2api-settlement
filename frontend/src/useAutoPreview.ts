import { useEffect, useRef, useState } from "react";
import { api, fromInput } from "./api";
import type { Preview } from "./types";

export function useAutoPreview({ ids, expenseIds, end, revision, paused = false, onResult }: {
  ids: number[]; expenseIds: number[]; end: string; revision: string;
  paused?: boolean; onResult?: (preview: Preview) => void;
}) {
  let cutoff = "";
  try { if (end) cutoff = fromInput(end); } catch { /* Incomplete date input. */ }
  const key = JSON.stringify([ids, expenseIds, cutoff, revision]);
  const latestKey = useRef(key);
  latestKey.current = key;
  const [state, setState] = useState<{ key: string; preview: Preview | null; error: string; loading: boolean }>({ key: "", preview: null, error: "", loading: true });
  const [attempt, setAttempt] = useState(0);
  const generation = useRef(0);
  const previousEnd = useRef(end);
  const callback = useRef(onResult);
  callback.current = onResult;
  const refresh = async () => {
    ++generation.current;
    setState(s => ({ ...s, loading: true, error: "" }));
    setAttempt(x => x + 1);
  };
  useEffect(() => {
    const token = ++generation.current;
    let expiry: ReturnType<typeof setTimeout> | undefined;
    const changedEnd = previousEnd.current !== end;
    previousEnd.current = end;
    if (paused) return;
    if (!cutoff) {
      setState({ key, preview: null, error: "请填写有效的结束时间", loading: false });
      return;
    }
    setState({ key, preview: null, error: "", loading: true });
    const timer = setTimeout(async () => {
      try {
        const result = await api<Preview>("/previews", "POST", { ids, expense_ids: expenseIds, end: cutoff });
        if (token !== generation.current || latestKey.current !== key) return;
        setState({ key, preview: result, error: "", loading: false });
        callback.current?.(result);
        expiry = setTimeout(() => { if (token === generation.current) void refresh(); }, Math.max(0, Date.parse(result.expires_at) - Date.now()));
      } catch (error) {
        if (token === generation.current && latestKey.current === key) setState({ key, preview: null, error: (error as Error).message, loading: false });
      }
    }, changedEnd ? 300 : 0);
    return () => { ++generation.current; clearTimeout(timer); clearTimeout(expiry); };
  }, [key, attempt, paused]);
  return {
    preview: state.key === key && !state.loading ? state.preview : null,
    loading: state.key !== key || state.loading,
    error: state.key === key ? state.error : "",
    refresh,
  };
}
