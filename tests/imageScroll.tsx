import { useEffect, useMemo, useRef, useState } from "react";
import { createRoot } from "react-dom/client";
import { mockIPC } from "@tauri-apps/api/mocks";
import { watchImage } from "../src/lib/imageLoading";
import "../src/styles.css";
const png = Array.from(atob("iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+aOuoAAAAASUVORK5CYII="), (c) => c.charCodeAt(0));
mockIPC((cmd) => cmd === "fetch_image" ? png : undefined);
const html = '<img src="https://cdnfile.sspai.com/scroll-test"><p style="height:2000px">Scroll regression</p>';
function App() {
  const [scrolled, setScrolled] = useState(false);
  const ref = useRef<HTMLDivElement>(null);
  const bodyMarkup = useMemo(() => ({__html:html}), []);
  useEffect(() => {
    const img = ref.current!.querySelector("img")!;
    return watchImage(img, null, {failed:"Failed",retry:"Retry",open:"Open"}, () => {});
  }, []);
  return <div style={{height:300,overflow:"auto"}} onScroll={() => setScrolled(true)}>
    <p>{scrolled ? "Scrolled" : "Top"}</p>
    <div ref={ref} dangerouslySetInnerHTML={bodyMarkup} />
  </div>;
}
createRoot(document.getElementById("root")!).render(<App/>);
Object.assign(window, { runImageScrollCheck: async () => {
  const waitFor = async (condition: () => boolean) => {
    const deadline = Date.now() + 3000;
    while (!condition()) {
      if (Date.now() > deadline) throw new Error("Scroll regression timed out");
      await new Promise((resolve) => setTimeout(resolve, 20));
    }
  };
  await waitFor(() => (document.querySelector("img")?.naturalWidth ?? 0) > 0);
  const img = document.querySelector("img")!;
  const src = img.src;
  const scroller = document.querySelector<HTMLElement>("#root > div")!;
  scroller.scrollTop = 100;
  await waitFor(() => scroller.firstElementChild?.textContent === "Scrolled");
  scroller.scrollTop = 0;
  await new Promise((resolve) => requestAnimationFrame(() => requestAnimationFrame(resolve)));
  if (!img.isConnected || document.querySelector("img") !== img || img.src !== src || img.naturalWidth === 0) {
    throw new Error("Scroll replaced a recovered image");
  }
  return "Recovered image node and data URL survive scroll rerenders";
} });
