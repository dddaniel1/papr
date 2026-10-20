import { useRef } from "react";
import { useTranslation } from "react-i18next";
import { imageUrl } from "../lib/imageLoading";
import type { ArticleSummary } from "../types";
import ArticleImage from "./ArticleImage";

interface Props {
  article: ArticleSummary;
  onOpen: (srcs: string[], index: number) => void;
}

export default function TimelineMedia({ article, onOpen }: Props) {
  const { t } = useTranslation();
  const root = useRef<HTMLDivElement>(null);
  const urls = [...new Set((article.imageUrls?.length ? article.imageUrls : [article.imageUrl])
    .flatMap((src) => {
      const resolved = src && imageUrl(src, article.url);
      return resolved ? [resolved] : [];
    }))];
  if (!urls.length) return null;

  return (
    <div className="timeline-media" data-count={Math.min(urls.length, 4)} ref={root}
      onClick={(event) => event.stopPropagation()}>
      {urls.slice(0, 4).map((src, index) => (
        <div className="timeline-media-tile" key={src}>
          <ArticleImage src={src} pageUrl={article.url} alt="" loading="lazy" />
          <button type="button" className="timeline-media-open"
            aria-label={t("articleList.openImage", { index: index + 1, count: urls.length })}
            onClick={() => {
              const images = root.current?.querySelectorAll("img");
              // Keep recovered/proxied sources; hidden images recover in the viewer.
              onOpen(urls.map((url, i) => {
                const img = images?.[i];
                return img?.naturalWidth ? img.currentSrc || img.src : url;
              }), index);
            }}>
            {index === 3 && urls.length > 4 && (
              <span className="timeline-media-more" aria-hidden="true">+{urls.length - 4}</span>
            )}
          </button>
        </div>
      ))}
    </div>
  );
}
