import { useEffect, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { useQueryClient } from "@tanstack/react-query";
import * as api from "../api";
import { useArticleActions } from "../hooks/articleActions";
import { useFocusTrap } from "../hooks/useFocusTrap";
import { NO_AUTOCORRECT } from "../lib/inputProps";
import type { Feed, Folder } from "../types";
import Icon from "./Icon";

interface Props {
  feed: Feed;
  folders: Folder[];
  onClose: () => void;
  onToast: (msg: string) => void;
}

export default function EditFeedDialog({
  feed,
  folders,
  onClose,
  onToast,
}: Props) {
  const { t } = useTranslation();
  const qc = useQueryClient();
  const actions = useArticleActions();
  const dialogRef = useRef<HTMLDivElement>(null);
  const titleInputRef = useRef<HTMLInputElement>(null);
  useFocusTrap(dialogRef);

  const [title, setTitle] = useState(feed.title);
  const [feedUrl, setFeedUrl] = useState(feed.feedUrl);
  const [folderId, setFolderId] = useState<number | null>(feed.folderId);
  const [openMode, setOpenMode] = useState<"default" | "reader" | "extracted" | "web">(
    feed.openMode ?? "default",
  );
  const [copied, setCopied] = useState(false);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  // Focus and select title on open for fast editing
  useEffect(() => {
    titleInputRef.current?.select();
  }, []);

  // Escape key closes modal
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") {
        e.stopPropagation();
        onClose();
      }
    };
    window.addEventListener("keydown", onKey, true);
    return () => window.removeEventListener("keydown", onKey, true);
  }, [onClose]);

  const copyUrl = async () => {
    try {
      await navigator.clipboard.writeText(feedUrl);
      setCopied(true);
      setTimeout(() => setCopied(false), 2000);
    } catch {
      // ignore clipboard error
    }
  };

  const submit = async () => {
    const trimmedTitle = title.trim();
    if (!trimmedTitle) {
      setError(t("editFeed.errorEmptyTitle"));
      return;
    }
    const trimmedUrl = feedUrl.trim();
    if (!trimmedUrl) {
      setError(t("editFeed.errorEmptyUrl"));
      return;
    }

    setBusy(true);
    setError(null);
    try {
      await api.updateFeed(
        feed.id,
        trimmedTitle,
        trimmedUrl,
        folderId,
        openMode === "default" ? null : openMode,
      );
      await qc.invalidateQueries({ queryKey: ["feeds"] });
      actions.refreshAfterBulk();
      onToast(t("editFeed.toastSaved"));
      onClose();
    } catch (e: unknown) {
      const msg = String((e as { message?: string })?.message ?? e);
      if (msg.includes("feedAlreadyExists")) {
        setError(t("editFeed.errorUrlExists"));
      } else {
        setError(msg);
      }
    } finally {
      setBusy(false);
    }
  };

  const openModeOptions = [
    { value: "default", label: t("settings.subscriptions.openDefault") },
    { value: "reader", label: t("settings.subscriptions.openReader") },
    { value: "extracted", label: t("settings.subscriptions.openExtracted") },
    { value: "web", label: t("settings.subscriptions.openWeb") },
  ];

  return (
    <div className="modal-backdrop" onClick={onClose}>
      <div
        className="modal"
        ref={dialogRef}
        role="dialog"
        aria-modal="true"
        aria-labelledby="edit-feed-dialog-title"
        onClick={(e) => e.stopPropagation()}
        style={{ width: 440 }}
      >
        <h2 id="edit-feed-dialog-title">{t("editFeed.dialogTitle")}</h2>

        <div style={{ display: "flex", flexDirection: "column", gap: 12, marginTop: 14 }}>
          {/* Feed Title */}
          <div>
            <label
              style={{
                display: "block",
                fontSize: 12,
                fontWeight: 500,
                color: "var(--muted)",
                marginBottom: 5,
              }}
            >
              {t("editFeed.titleLabel")}
            </label>
            <input
              ref={titleInputRef}
              className="modal-input"
              value={title}
              disabled={busy}
              placeholder={t("sidebar.feedNamePlaceholder")}
              aria-label={t("editFeed.titleLabel")}
              {...NO_AUTOCORRECT}
              onChange={(e) => {
                setTitle(e.target.value);
                if (error) setError(null);
              }}
              onKeyDown={(e) => {
                if (e.key === "Enter" && !e.nativeEvent.isComposing) submit();
              }}
              style={{ margin: 0 }}
            />
          </div>

          {/* Folder */}
          <div>
            <label
              style={{
                display: "block",
                fontSize: 12,
                fontWeight: 500,
                color: "var(--muted)",
                marginBottom: 5,
              }}
            >
              {t("editFeed.folderLabel")}
            </label>
            <select
              className="s-select"
              value={folderId ?? "none"}
              disabled={busy}
              onChange={(e) => {
                const val = e.target.value;
                setFolderId(val === "none" ? null : Number(val));
              }}
              style={{ width: "100%", height: 36, borderRadius: 8, padding: "0 11px" }}
            >
              <option value="none">{t("sidebar.uncategorized")}</option>
              {folders.map((fo) => (
                <option key={fo.id} value={fo.id}>
                  {fo.name}
                </option>
              ))}
            </select>
          </div>

          {/* Open Mode */}
          <div>
            <label
              style={{
                display: "block",
                fontSize: 12,
                fontWeight: 500,
                color: "var(--muted)",
                marginBottom: 5,
              }}
            >
              {t("settings.subscriptions.openMode")}
            </label>
            <select
              className="s-select"
              value={openMode}
              disabled={busy}
              onChange={(e) =>
                setOpenMode(e.target.value as "default" | "reader" | "extracted" | "web")
              }
              style={{ width: "100%", height: 36, borderRadius: 8, padding: "0 11px" }}
            >
              {openModeOptions.map((o) => (
                <option key={o.value} value={o.value}>
                  {o.label}
                </option>
              ))}
            </select>
          </div>

          {/* Feed URL */}
          <div>
            <label
              style={{
                display: "block",
                fontSize: 12,
                fontWeight: 500,
                color: "var(--muted)",
                marginBottom: 5,
              }}
            >
              {t("editFeed.urlLabel")}
            </label>
            <div style={{ display: "flex", gap: 6 }}>
              <input
                className="modal-input"
                value={feedUrl}
                disabled={busy}
                placeholder="https://"
                aria-label={t("editFeed.urlLabel")}
                {...NO_AUTOCORRECT}
                onChange={(e) => {
                  setFeedUrl(e.target.value);
                  if (error) setError(null);
                }}
                onKeyDown={(e) => {
                  if (e.key === "Enter" && !e.nativeEvent.isComposing) submit();
                }}
                style={{
                  margin: 0,
                  flex: 1,
                  fontFamily: "var(--mono)",
                  fontSize: 12,
                }}
              />
              <button
                type="button"
                className="s-btn"
                onClick={copyUrl}
                disabled={!feedUrl}
                title={t("editFeed.copyUrl")}
                style={{ height: 36, padding: "0 12px", whiteSpace: "nowrap" }}
              >
                {copied ? (
                  <span style={{ color: "var(--accent)", display: "inline-flex", alignItems: "center", gap: 4 }}>
                    <Icon name="check" size={13} /> {t("editFeed.copied")}
                  </span>
                ) : (
                  <span style={{ display: "inline-flex", alignItems: "center", gap: 4 }}>
                    <Icon name="copy" size={13} /> {t("editFeed.copy")}
                  </span>
                )}
              </button>
            </div>
          </div>
        </div>

        {error && <div className="modal-error" style={{ marginTop: 8 }}>{error}</div>}

        <div className="modal-actions" style={{ marginTop: 18 }}>
          <button className="s-btn" onClick={onClose} disabled={busy}>
            {t("common.cancel")}
          </button>
          <button
            className="s-btn primary"
            onClick={submit}
            disabled={busy || !title.trim() || !feedUrl.trim()}
          >
            {busy ? t("common.saving", "保存中…") : t("common.save")}
          </button>
        </div>
      </div>
    </div>
  );
}
