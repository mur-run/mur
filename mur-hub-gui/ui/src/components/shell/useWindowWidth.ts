import { useEffect, useState } from "react";

export function useWindowWidth(): number {
  const [width, setWidth] = useState(() => (typeof window === "undefined" ? 0 : window.innerWidth));
  useEffect(() => {
    function onResize() {
      setWidth(window.innerWidth);
    }
    window.addEventListener("resize", onResize);
    return () => window.removeEventListener("resize", onResize);
  }, []);
  return width;
}
