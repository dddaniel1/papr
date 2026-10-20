import { useEffect, useState } from "react";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import { useTranslation } from "react-i18next";
import * as api from "../api";
import { NO_AUTOCORRECT } from "../lib/inputProps";
import { errorText } from "../lib/errors";
import ServiceConnection from "./ServiceConnection";
import Row from "./SettingsRow";

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
    return <p className="service-note" role="status">{t("settings.archive.loading")}</p>;
  }
  if (!settings.data) {
    return (
      <div className="settings-group">
        <p className="service-note" role="alert">{t("settings.archive.loadFailed")}</p>
        <button className="s-btn" onClick={() => void settings.refetch()} disabled={settings.isFetching}>
          {settings.isFetching ? t("common.loading") : t("common.retry")}
        </button>
      </div>
    );
  }

  const feedbackFor = (service: api.ArchiveService) => feedback?.service === service && (
    <p className="service-feedback" role={feedback.error ? "alert" : "status"}>
      {t(`settings.archive.${feedback.key}`)}
      {feedback.detail && <> {feedback.detail}</>}
    </p>
  );

  return (
    <div className="settings-group">
      <h3 className="settings-group-title">{t("settings.services.archiving")}</h3>
      <ServiceConnection service="obsidian" name="Obsidian" description={t("settings.archive.obsidianHint")} connected={settings.data.obsidian.configured} status={t(settings.data.obsidian.configured ? "settings.archive.configured" : "settings.services.notConfigured")}>
        <form className="service-form" onSubmit={(e) => { e.preventDefault(); void save("obsidian"); }}>
          <Row label={t("settings.archive.vaultPath")} desc={t("settings.archive.vaultPathHint")}>
          <input
            id="archive-vault-path"
            className="s-text-input"
            {...NO_AUTOCORRECT}
            value={vaultPath}
            onChange={(e) => setVaultPath(e.target.value)}
            placeholder={t("settings.archive.vaultPathPlaceholder")}
            disabled={busy !== null}
            required
          />
          </Row>
          <div className="service-actions">
            <button className="s-btn" type="button" onClick={() => void chooseVault()} disabled={busy !== null}>
              {t("settings.archive.chooseVault")}
            </button>
          </div>
          <Row label={t("settings.archive.folder")} desc={t("settings.archive.folderHint")}>
          <input
            id="archive-folder"
            className="s-text-input"
            {...NO_AUTOCORRECT}
            value={folder}
            onChange={(e) => setFolder(e.target.value)}
            placeholder={t("settings.archive.folderPlaceholder")}
            disabled={busy !== null}
          />
          </Row>
          <div className="service-actions">
            <button className="s-btn primary" type="submit" disabled={busy !== null || !vaultPath.trim()}>
              {busy === "obsidian" ? t("settings.archive.working") : t("settings.archive.saveVerify")}
            </button>
            {settings.data.obsidian.configured && (
              <>
                <button className="s-btn" type="button" onClick={() => void save("obsidian", true)} disabled={busy !== null}>
                  {t("settings.archive.disconnect")}
                </button>
                <span className="service-note">{t("settings.archive.configured")}</span>
              </>
            )}
          </div>
          {feedbackFor("obsidian")}
        </form>
      </ServiceConnection>

      <ServiceConnection service="notion" name="Notion" description={t("settings.archive.notionHint")} connected={settings.data.notion.configured} status={t(settings.data.notion.configured ? "settings.archive.configured" : "settings.services.notConfigured")}>
        <form className="service-form" onSubmit={(e) => { e.preventDefault(); void save("notion"); }}>
          <Row label={t("settings.archive.token")} desc={t("settings.archive.tokenHint")}>
          <input
            id="archive-notion-token"
            className="s-text-input"
            type="password"
            {...NO_AUTOCORRECT}
            autoComplete="new-password"
            value={token}
            onChange={(e) => setToken(e.target.value)}
            placeholder={t(settings.data.notion.tokenConfigured ? "settings.archive.tokenKeepPlaceholder" : "settings.archive.tokenPlaceholder")}
            disabled={busy !== null}
            required={!settings.data.notion.tokenConfigured}
          />
          </Row>
          <Row label={t("settings.archive.targetType")}>
          <select
            id="archive-target-type"
            className="s-select"
            value={targetType}
            onChange={(e) => setTargetType(e.target.value as "page" | "database")}
            disabled={busy !== null}
          >
            <option value="page">{t("settings.archive.page")}</option>
            <option value="database">{t("settings.archive.database")}</option>
          </select>
          </Row>
          <Row label={t("settings.archive.targetId")} desc={t("settings.archive.targetHint")}>
          <input
            id="archive-target-id"
            className="s-text-input"
            {...NO_AUTOCORRECT}
            value={targetId}
            onChange={(e) => setTargetId(e.target.value)}
            placeholder={t("settings.archive.targetIdPlaceholder")}
            disabled={busy !== null}
            required
          />
          </Row>
          <div className="service-actions">
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
                <span className="service-note">{t("settings.archive.configured")}</span>
              </>
            )}
          </div>
          {feedbackFor("notion")}
        </form>
      </ServiceConnection>
      <div className="service-notes">
        <p className="service-note">{t("settings.archive.overwriteHint")}</p>
        <p className="service-note">{t("settings.archive.imagesHint")}</p>
      </div>
    </div>
  );
}
