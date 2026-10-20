import { useMutation, useQuery } from "@tanstack/react-query";
import { useTranslation } from "react-i18next";
import * as api from "../api";
import { reportError, toast } from "../toast";
import Icon from "./Icon";

export default function ArchiveButtons({ articleId }: { articleId: number }) {
  const settings = useQuery({
    queryKey: ["archive-settings"],
    queryFn: api.getArchiveSettings,
  });

  return (
    <>
      {settings.data?.obsidian.configured && (
        <ArchiveButton key={`obsidian-${articleId}`} articleId={articleId} service="obsidian" />
      )}
      {settings.data?.notion.configured && (
        <ArchiveButton key={`notion-${articleId}`} articleId={articleId} service="notion" />
      )}
    </>
  );
}

function ArchiveButton({ articleId, service }: { articleId: number; service: api.ArchiveService }) {
  const { t } = useTranslation();
  const name = service === "obsidian" ? "Obsidian" : "Notion";
  const save = useMutation({
    mutationFn: () => api.archiveArticle(articleId, service),
    onSuccess: () => toast.show(t("reader.archiveSaved", { service: name })),
    onError: reportError,
  });
  const label = save.isPending
    ? t("reader.archiveSaving", { service: name })
    : t(service === "obsidian" ? "reader.saveToObsidian" : "reader.saveToNotion");

  return (
    <button
      type="button"
      className="tb-btn"
      title={label}
      aria-label={label}
      aria-busy={save.isPending}
      disabled={save.isPending}
      onClick={() => save.mutate()}
    >
      <Icon name={save.isPending ? "refresh" : service} size={16} className={save.isPending ? "spinning" : undefined} />
    </button>
  );
}
