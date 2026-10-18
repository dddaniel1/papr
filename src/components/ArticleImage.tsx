import { useEffect, useRef, type ImgHTMLAttributes } from "react";
import { useTranslation } from "react-i18next";
import { openUrl } from "@tauri-apps/plugin-opener";
import { imageUrl, watchImage } from "../lib/imageLoading";
import { reportError } from "../toast";

type Props = ImgHTMLAttributes<HTMLImageElement> & { src: string; pageUrl?: string | null };

export default function ArticleImage({ src, pageUrl, ...props }: Props) {
  const ref = useRef<HTMLImageElement>(null);
  const { t } = useTranslation();
  const resolved = imageUrl(src, pageUrl) ?? "";
  useEffect(() => {
    const img = ref.current;
    if (!img) return;
    img.src = resolved;
    delete img.dataset.paprSrc;
    return watchImage(img, pageUrl, {
      failed: t("reader.imageLoadFailed"),
      retry: t("common.retry"),
      open: t("reader.openOriginalImage"),
    }, (url) => { openUrl(url).catch(reportError); });
  }, [resolved, pageUrl, t]);
  return <img {...props} ref={ref} src={resolved} referrerPolicy="no-referrer" />;
}
