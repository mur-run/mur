import { useEffect, useRef, useState } from "react";
import { listen } from "@tauri-apps/api/event";

const STREAM_TAIL_CHARS = 20_000;

export default function StreamTail({ pid }: { pid: number }) {
  const [buf, setBuf] = useState("");
  const ref = useRef<HTMLPreElement>(null);

  useEffect(() => {
    setBuf("");
    const un = listen<{ pid: number; delta: string }>("panel-stream", (e) => {
      if (e.payload.pid !== pid) return;
      setBuf((b) => (b + e.payload.delta).slice(-STREAM_TAIL_CHARS));
    });
    return () => {
      un.then((f) => f());
    };
  }, [pid]);

  useEffect(() => {
    ref.current?.scrollTo(0, ref.current.scrollHeight);
  }, [buf]);

  if (!buf) {
    return (
      <p className="panel-empty">
        No live stream yet — type <code>/panel stream on</code> in murmur.
      </p>
    );
  }

  return (
    <pre ref={ref} className="stream-tail">
      {buf}
    </pre>
  );
}
