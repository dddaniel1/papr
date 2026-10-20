import { useEffect, useState } from "react";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import { useTranslation } from "react-i18next";
import * as api from "../api";
import { NO_AUTOCORRECT } from "../lib/inputProps";
import { errorText } from "../lib/errors";
import ServiceConnection from "./ServiceConnection";

const hintStyle = { fontSize: 12, color: "var(--muted)", lineHeight: 1.5 };

export default function ArchiveSettings() {
  const { t } = useTranslation();
  const qc = useQueryClient();
  const settings = useQuery({
    queryKey: ["archive-settings"],
    queryFn: api.getArchiveSettings,
    refetchOnWindowFocus: false,
  });
  const [vaultPath, setVaultPath] = useState("");
  const [folder, setFolder] = useState("");
  const [token, setToken] = useState("");
  const [targetType, setTargetType] = useState<"page" | "database">("page");
  const [targetId, setTargetId] = useState("");
  const [busy, setBusy] = useState<api.ArchiveService | null>(null);
  const [feedback, setFeedback] = useState<{
    service: api.ArchiveService;
    key: string;
    error: boolean;
    detail?: string;
  } | null>(null);

  const obsidian = settings.data?.obsidian;
  const notion = settings.data?.notion;
  useEffect(() => {
    if (obsidian) {
      setVaultPath(obsidian.vaultPath);
      setFolder(obsidian.folder);
    }
  }, [obsidian?.vaultPath, obsidian?.folder]);
  useEffect(() => {
    if (notion) {
      setTargetType(notion.targetType);
      setTargetId(notion.targetId);
    }
  }, [notion?.targetType, notion?.targetId]);

  const safeDetail = (error: unknown) => {
    const detail = errorText(error);
    return token.trim() ? detail.split(token.trim()).join("[redacted]") : detail;
  };

  const save = async (service: api.ArchiveService, disconnect = false) => {
    setBusy(service);
    setFeedback(null);
    try {
      const updated = disconnect
        ? await api.disconnectArchiveService(service)
        : service === "obsidian"
          ? await api.configureObsidian(vaultPath.trim(), folder.trim())
          : await api.configureNotion(token.trim(), targetType, targetId.trim());
      qc.setQueryData(["archive-settings"], updated);
      if (service === "notion") setToken("");
      setFeedback({ service, key: disconnect ? "disconnected" : "saved", error: false });
      await qc.invalidateQueries({ queryKey: ["archive-settings"] });
    } catch (error) {
      setFeedback({ service, key: disconnect ? "disconnectFailed" : "saveFailed", error: true, detail: safeDetail(error) });
    } finally {
      setBusy(null);
    }
  };

  const chooseVault = async () => {
    setBusy("obsidian");
    setFeedback(null);
    try {
      const path = await api.chooseObsidianVault();
      if (path !== null) setVaultPath(path);
    } catch (error) {
      setFeedback({ service: "obsidian", key: "chooseFailed", error: true, detail: safeDetail(error) });
    } finally {
      setBusy(null);
    }
  };

  if (settings.isPending) {
    return <p className="modal-hint" style={hintStyle} role="status">{t("settings.archive.loading")}</p>;
  }
  if (!settings.data) {
    return (
      <div className="settings-group">
        <p className="modal-hint" style={hintStyle} role="alert">{t("settings.archive.loadFailed")}</p>
        <button className="s-btn" onClick={() => void settings.refetch()} disabled={settings.isFetching}>
          {settings.isFetching ? t("common.loading") : t("common.retry")}
        </button>
      </div>
    );
  }

  const feedbackFor = (service: api.ArchiveService) => feedback?.service === service && (
    <p className="modal-hint" style={hintStyle} role={feedback.error ? "alert" : "status"}>
      {t(`settings.archive.${feedback.key}`)}
      {feedback.detail && <> {feedback.detail}</>}
    </p>
  );
  const formStyle = { display: "flex", flexDirection: "column", gap: 8 } as const;
  const inputStyle = { margin: 0 };

  return (
    <div className="settings-group">
      <h3 className="settings-group-title">{t("settings.services.archiving")}</h3>
      <ServiceConnection service="obsidian" name="Obsidian" description={t("settings.archive.obsidianHint")} connected={settings.data.obsidian.configured} status={t(settings.data.obsidian.configured ? "settings.archive.configured" : "settings.services.notConfigured")}>
        <form style={formStyle} onSubmit={(e) => { e.preventDefault(); void save("obsidian"); }}>
          <label htmlFor="archive-vault-path" className="settings-row-label">{t("settings.archive.vaultPath")}</label>
          <input
            id="archive-vault-path"
            className="modal-input"
            style={inputStyle}
            {...NO_AUTOCORRECT}
            value={vaultPath}
            onChange={(e) => setVaultPath(e.target.value)}
            placeholder={t("settings.archive.vaultPathPlaceholder")}
            aria-describedby="archive-vault-hint"
            disabled={busy !== null}
            required
          />
          <div>
            <button className="s-btn" type="button" onClick={() => void chooseVault()} disabled={busy !== null}>
              {t("settings.archive.chooseVault")}
            </button>
          </div>
          <p id="archive-vault-hint" className="modal-hint" style={{ ...hintStyle, margin: 0 }}>{t("settings.archive.vaultPathHint")}</p>
          <label htmlFor="archive-folder" className="settings-row-label">{t("settings.archive.folder")}</label>
          <input
            id="archive-folder"
            className="modal-input"
            style={inputStyle}
            {...NO_AUTOCORRECT}
            value={folder}
            onChange={(e) => setFolder(e.target.value)}
            placeholder={t("settings.archive.folderPlaceholder")}
            aria-describedby="archive-folder-hint"
            disabled={busy !== null}
          />
          <p id="archive-folder-hint" className="modal-hint" style={{ ...hintStyle, margin: 0 }}>{t("settings.archive.folderHint")}</p>
          <div style={{ display: "flex", alignItems: "center", flexWrap: "wrap", gap: 8, marginTop: 4 }}>
            <button className="s-btn primary" type="submit" disabled={busy !== null || !vaultPath.trim()}>
              {busy === "obsidian" ? t("settings.archive.working") : t("settings.archive.saveVerify")}
            </button>
            {settings.data.obsidian.configured && (
              <>
                <button className="s-btn" type="button" onClick={() => void save("obsidian", true)} disabled={busy !== null}>
                  {t("settings.archive.disconnect")}
                </button>
                <span className="modal-hint" style={hintStyle}>{t("settings.archive.configured")}</span>
              </>
            )}
          </div>
          {feedbackFor("obsidian")}
        </form>
      </ServiceConnection>

      <ServiceConnection service="notion" name="Notion" description={t("settings.archive.notionHint")} connected={settings.data.notion.configured} status={t(settings.data.notion.configured ? "settings.archive.configured" : "settings.services.notConfigured")}>
        <form style={formStyle} onSubmit={(e) => { e.preventDefault(); void save("notion"); }}>
          <label htmlFor="archive-notion-token" className="settings-row-label">{t("settings.archive.token")}</label>
          <input
            id="archive-notion-token"
            className="modal-input"
            style={inputStyle}
            type="password"
            {...NO_AUTOCORRECT}
            autoComplete="new-password"
            value={token}
            onChange={(e) => setToken(e.target.value)}
            placeholder={t(settings.data.notion.tokenConfigured ? "settings.archive.tokenKeepPlaceholder" : "settings.archive.tokenPlaceholder")}
            aria-describedby="archive-token-hint"
            disabled={busy !== null}
            required={!settings.data.notion.tokenConfigured}
          />
          <p id="archive-token-hint" className="modal-hint" style={{ ...hintStyle, margin: 0 }}>{t("settings.archive.tokenHint")}</p>
          <label htmlFor="archive-target-type" className="settings-row-label">{t("settings.archive.targetType")}</label>
          <select
            id="archive-target-type"
            className="modal-input"
            style={inputStyle}
            value={targetType}
            onChange={(e) => setTargetType(e.target.value as "page" | "database")}
            disabled={busy !== null}
          >
            <option value="page">{t("settings.archive.page")}</option>
            <option value="database">{t("settings.archive.database")}</option>
          </select>
          <label htmlFor="archive-target-id" className="settings-row-label">{t("settings.archive.targetId")}</label>
          <input
            id="archive-target-id"
            className="modal-input"
            style={inputStyle}
            {...NO_AUTOCORRECT}
            value={targetId}
            onChange={(e) => setTargetId(e.target.value)}
            placeholder={t("settings.archive.targetIdPlaceholder")}
            aria-describedby="archive-target-hint"
            disabled={busy !== null}
            required
          />
          <p id="archive-target-hint" className="modal-hint" style={{ ...hintStyle, margin: 0 }}>{t("settings.archive.targetHint")}</p>
          <div style={{ display: "flex", alignItems: "center", flexWrap: "wrap", gap: 8, marginTop: 4 }}>
            <button
              className="s-btn primary"
              type="submit"
              disabled={busy !== null || !targetId.trim() || (!token.trim() && !settings.data.notion.tokenConfigured)}
            >
              {busy === "notion" ? t("settings.archive.working") : t("settings.archive.saveVerify")}
            </button>
            {settings.data.notion.configured && (
              <>
                <button className="s-btn" type="button" onClick={() => void save("notion", true)} disabled={busy !== null}>
                  {t("settings.archive.disconnect")}
                </button>
                <span className="modal-hint" style={hintStyle}>{t("settings.archive.configured")}</span>
              </>
            )}
          </div>
          {feedbackFor("notion")}
        </form>
      </ServiceConnection>
      <div>
        <p className="modal-hint" style={{ ...hintStyle, marginTop: 14 }}>{t("settings.archive.overwriteHint")}</p>
        <p className="modal-hint" style={hintStyle}>{t("settings.archive.imagesHint")}</p>
      </div>
    </div>
  );
}
