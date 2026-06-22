import ReactMarkdown from "react-markdown";
import remarkGfm from "remark-gfm";

// Shared renderer for agent/chat message bodies. react-markdown does NOT emit
// raw HTML by default (no rehype-raw), so remote content can't inject markup.
// Links open in the OS browser via the shell plugin rather than navigating the
// WebView away from the app.
import { open as openExternal } from "@tauri-apps/plugin-shell";

interface Props {
  children: string;
  className?: string;
}

export function Markdown({ children, className }: Props) {
  return (
    <div className={`md${className ? ` ${className}` : ""}`}>
      <ReactMarkdown
        remarkPlugins={[remarkGfm]}
        components={{
          a: ({ href, children }) => (
            <a
              href={href}
              onClick={(e) => {
                e.preventDefault();
                if (href) void openExternal(href).catch(() => {});
              }}
            >
              {children}
            </a>
          ),
        }}
      >
        {children}
      </ReactMarkdown>
    </div>
  );
}
