import { useEffect, useMemo, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { openUrl } from "@tauri-apps/plugin-opener";
import * as api from "../api";
import { useFocusTrap } from "../hooks/useFocusTrap";
import { renderMarkdown } from "../lib/markdown";
import { errorText } from "../lib/errors";
import Icon from "./Icon";

export type AiMode = "ask" | "digest";

export default function AiDialog({ initialMode, onClose, onConfigure }: {
  initialMode: AiMode;
  onClose: () => void;
  onConfigure: () => void;
}) {
  const { t } = useTranslation();
  const ref = useRef<HTMLDivElement>(null);
  useFocusTrap(ref);
  const [mode, setMode] = useState(initialMode);
  const [question, setQuestion] = useState("");
  const [text, setText] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");
  const generation = useRef(0);
  const running = useRef(false);
  useEffect(() => () => { ++generation.current; }, []);
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") { e.stopImmediatePropagation(); onClose(); }
    };
    window.addEventListener("keydown", onKey, true);
    return () => window.removeEventListener("keydown", onKey, true);
  }, [onClose]);

  const run = async () => {
    if (running.current || (mode === "ask" && !question.trim())) return;
    const id = ++generation.current;
    running.current = true;
    setBusy(true);
    setText("");
    setError("");
    let streamError = false;
    const onToken: Parameters<typeof api.aiDigest>[0] = (event) => {
      if (id !== generation.current) return;
      if (event.type === "delta") setText((old) => old + event.data);
      if (event.type === "error") { streamError = true; setError(event.data); }
    };
    try {
      if (mode === "ask") await api.aiAsk(question.trim(), onToken);
      else await api.aiDigest(onToken);
    } catch (e) {
      if (id === generation.current && !streamError) setError(errorText(e));
    } finally {
      running.current = false;
      if (id === generation.current) setBusy(false);
    }
  };
  const html = useMemo(() => renderMarkdown(text), [text]);
  return (
    <div className="modal-backdrop" onClick={onClose}>
      <div className="modal ai-dialog" ref={ref} role="dialog" aria-modal="true" aria-labelledby="ai-dialog-title" onClick={(e) => e.stopPropagation()}>
        <div className="ai-dialog-head">
          <h2 id="ai-dialog-title">{t("ai.title")}</h2>
          <button className="tb-btn" aria-label={t("common.close")} onClick={onClose}><Icon name="x" /></button>
        </div>
        <div className="s-seg" role="group" aria-label={t("ai.mode")}>
          {(["ask", "digest"] as const).map((value) => (
            <button key={value} disabled={busy} aria-pressed={mode === value} className={mode === value ? "on" : ""} onClick={() => { setMode(value); setText(""); setError(""); }}>{t(`ai.${value}`)}</button>
          ))}
        </div>
        <p className="modal-hint">{t(mode === "ask" ? "ai.askHint" : "ai.digestHint")}</p>
        <form onSubmit={(e) => { e.preventDefault(); void run(); }}>
          {mode === "ask" && <input className="modal-input" aria-label={t("ai.question")} placeholder={t("ai.question")} value={question} disabled={busy} onChange={(e) => setQuestion(e.target.value)} />}
          <div className="ai-dialog-actions">
            <button className="s-btn primary" disabled={busy || (mode === "ask" && !question.trim())}>{busy ? t("ai.generating") : error ? t("common.retry") : t(mode === "ask" ? "ai.submit" : "ai.generate")}</button>
            <button type="button" className="s-btn" onClick={onConfigure}>{t("ai.configure")}</button>
          </div>
        </form>
        <div role="status" aria-live="polite" aria-busy={busy}>
          {error && <p className="ai-dialog-error" role="alert">{error}</p>}
          {busy && !text && <p>{t("ai.generating")}</p>}
        </div>
        {text && !error && <div className="ai-prose ai-dialog-result" onClick={(e) => {
          const a = (e.target as HTMLElement).closest("a");
          if (a?.href) { e.preventDefault(); void openUrl(a.href).catch(() => setError(t("ai.linkError"))); }
        }} dangerouslySetInnerHTML={{ __html: html }} />}
        {text && !error && <p className="modal-hint">{t("reader.aiDisclaimer")}</p>}
      </div>
    </div>
  );
}
