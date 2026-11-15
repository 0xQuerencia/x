import { StrictMode, useEffect, type PropsWithChildren } from "react";
import { createRoot } from "react-dom/client";
import Lenis from "lenis";
import { App } from "./App";
import "../style.css";
import "lenis/dist/lenis.css";

function SmoothScroll({ children }: PropsWithChildren) {
  useEffect(() => {
    const lenis = new Lenis({
      autoRaf: true,
      smoothWheel: true,
      lerp: 0.12,
      anchors: { offset: -90 },
      respectReducedMotion: true,
    });
    return () => lenis.destroy();
  }, []);
  return <>{children}</>;
}

const root = document.getElementById("root");
if (!root) throw new Error("Missing #root");
createRoot(root).render(<StrictMode><SmoothScroll><App /></SmoothScroll></StrictMode>);
