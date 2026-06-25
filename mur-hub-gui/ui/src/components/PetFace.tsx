// Built-in vector mascot for desktop pets. Renders offline with no image model,
// so each of the six style presets looks distinct and every expression reads
// clearly. When real LLM-rendered .webp art exists, PetApp uses that instead and
// this component is not shown.
//
// Distinctness comes from a per-preset palette + silhouette; expressiveness from
// a shared eyes/mouth/accent set keyed by the 12 canonical expression slots.
// Blink + breathe are CSS-only (see pet.css) so the component stays pure.

interface PetFaceProps {
  /** Style preset id, e.g. "chiikawa". Falls back to family, then default-blob. */
  presetId: string;
  /** chibi | pixel | live2d | polaroid — base silhouette. */
  family: string;
  /** One of the 12 canonical expression slots. */
  expression: string;
  size?: number;
  /** Idle blink/breathe animation. Off for static thumbnails (agent cards). */
  animate?: boolean;
}

type Shape = "ellipse" | "roundsquare" | "pixel" | "polaroid";
type Ears = "round" | "bow" | "antenna" | "curl" | "pixel" | "none";

interface Style {
  body: string;
  belly: string;
  cheek: string;
  outline: string;
  shape: Shape;
  ears: Ears;
}

// Per-preset look. Keep palettes soft and on-brand; each is visibly different.
const STYLES: Record<string, Style> = {
  "default-blob": { body: "#8fd6a6", belly: "#c8efd4", cheek: "#f3a8b8", outline: "#3f7a55", shape: "ellipse", ears: "curl" },
  chiikawa: { body: "#fbf6ec", belly: "#ffffff", cheek: "#f6b8c4", outline: "#caa97e", shape: "ellipse", ears: "round" },
  "sanrio-pastel": { body: "#ffd7e6", belly: "#fff0f5", cheek: "#ff9ec0", outline: "#d98aa6", shape: "ellipse", ears: "bow" },
  sumikko: { body: "#cfe3dc", belly: "#eef6f2", cheek: "#e8b6a0", outline: "#8aa79c", shape: "roundsquare", ears: "none" },
  "vtuber-soft": { body: "#d9cdf6", belly: "#f0eafc", cheek: "#f1a7d0", outline: "#9b86c9", shape: "ellipse", ears: "antenna" },
  "shimeji-retro": { body: "#7ad6c8", belly: "#bdeee6", cheek: "#f2b06a", outline: "#2e7d72", shape: "pixel", ears: "pixel" },
  "family-photo": { body: "#ffffff", belly: "#f4ead9", cheek: "#e9a98c", outline: "#b9a489", shape: "polaroid", ears: "none" },
};

const FAMILY_FALLBACK: Record<string, string> = {
  chibi: "default-blob",
  live2d: "vtuber-soft",
  pixel: "shimeji-retro",
  polaroid: "family-photo",
};

function resolveStyle(presetId: string, family: string): Style {
  return STYLES[presetId] ?? STYLES[FAMILY_FALLBACK[family] ?? "default-blob"] ?? STYLES["default-blob"];
}

// Expression → drawing recipe. eye/mouth are shared shapes; accent adds the
// little signal that makes the emotion unambiguous (teardrop, Zzz, sparkle…).
type Eye = "open" | "happy" | "wide" | "closed" | "teary" | "squint" | "dizzy";
type Mouth = "neutral" | "smile" | "grin" | "open" | "frown" | "o" | "flat";
type Accent = "none" | "tear" | "zzz" | "sparkle" | "think" | "bang" | "wave";

const EXPR: Record<string, { eye: Eye; mouth: Mouth; accent: Accent }> = {
  idle: { eye: "open", mouth: "neutral", accent: "none" },
  smile: { eye: "happy", mouth: "smile", accent: "none" },
  wave: { eye: "open", mouth: "grin", accent: "wave" },
  think: { eye: "squint", mouth: "flat", accent: "think" },
  wow: { eye: "wide", mouth: "o", accent: "bang" },
  sparkle: { eye: "happy", mouth: "grin", accent: "sparkle" },
  cry: { eye: "teary", mouth: "frown", accent: "tear" },
  sleep: { eye: "closed", mouth: "neutral", accent: "zzz" },
  peek: { eye: "squint", mouth: "smile", accent: "none" },
  talk_open: { eye: "open", mouth: "open", accent: "none" },
  talk_close: { eye: "open", mouth: "flat", accent: "none" },
  error: { eye: "dizzy", mouth: "frown", accent: "bang" },
};

function Eyes({ kind }: { kind: Eye }) {
  // Left/right eye centres.
  const lx = 38, rx = 62, cy = 52;
  const dark = "#2b2b35";
  switch (kind) {
    case "happy":
      return (
        <g stroke={dark} strokeWidth={3} strokeLinecap="round" fill="none">
          <path d={`M${lx - 6} ${cy} Q${lx} ${cy - 7} ${lx + 6} ${cy}`} />
          <path d={`M${rx - 6} ${cy} Q${rx} ${cy - 7} ${rx + 6} ${cy}`} />
        </g>
      );
    case "closed":
      return (
        <g stroke={dark} strokeWidth={3} strokeLinecap="round">
          <path d={`M${lx - 6} ${cy} Q${lx} ${cy + 5} ${lx + 6} ${cy}`} fill="none" />
          <path d={`M${rx - 6} ${cy} Q${rx} ${cy + 5} ${rx + 6} ${cy}`} fill="none" />
        </g>
      );
    case "squint":
      return (
        <g stroke={dark} strokeWidth={3} strokeLinecap="round">
          <line x1={lx - 6} y1={cy} x2={lx + 6} y2={cy} />
          <line x1={rx - 6} y1={cy} x2={rx + 6} y2={cy} />
        </g>
      );
    case "wide":
      return (
        <g fill="#fff" stroke={dark} strokeWidth={2}>
          <circle cx={lx} cy={cy} r={8} />
          <circle cx={rx} cy={cy} r={8} />
          <circle cx={lx} cy={cy} r={4} fill={dark} stroke="none" />
          <circle cx={rx} cy={cy} r={4} fill={dark} stroke="none" />
        </g>
      );
    case "teary":
      return (
        <g>
          <circle cx={lx} cy={cy} r={5} fill={dark} />
          <circle cx={rx} cy={cy} r={5} fill={dark} />
          <circle cx={lx + 2} cy={cy - 2} r={1.5} fill="#fff" />
          <circle cx={rx + 2} cy={cy - 2} r={1.5} fill="#fff" />
        </g>
      );
    case "dizzy":
      return (
        <g stroke={dark} strokeWidth={2.4} fill="none" strokeLinecap="round">
          <path d={`M${lx - 6} ${cy - 6} L${lx + 6} ${cy + 6} M${lx - 6} ${cy + 6} L${lx + 6} ${cy - 6}`} />
          <path d={`M${rx - 6} ${cy - 6} L${rx + 6} ${cy + 6} M${rx - 6} ${cy + 6} L${rx + 6} ${cy - 6}`} />
        </g>
      );
    default: // open
      return (
        <g fill={dark}>
          <circle cx={lx} cy={cy} r={5.5} />
          <circle cx={rx} cy={cy} r={5.5} />
          <circle cx={lx + 1.8} cy={cy - 1.8} r={1.6} fill="#fff" />
          <circle cx={rx + 1.8} cy={cy - 1.8} r={1.6} fill="#fff" />
        </g>
      );
  }
}

function Mouth({ kind }: { kind: Mouth }) {
  const cx = 50, cy = 66;
  const dark = "#2b2b35";
  switch (kind) {
    case "smile":
      return <path d={`M${cx - 8} ${cy} Q${cx} ${cy + 8} ${cx + 8} ${cy}`} fill="none" stroke={dark} strokeWidth={3} strokeLinecap="round" />;
    case "grin":
      return <path d={`M${cx - 9} ${cy - 1} Q${cx} ${cy + 11} ${cx + 9} ${cy - 1} Z`} fill="#a8455a" stroke={dark} strokeWidth={2} />;
    case "open":
      return <ellipse cx={cx} cy={cy + 2} rx={6} ry={7} fill="#a8455a" stroke={dark} strokeWidth={2} />;
    case "o":
      return <circle cx={cx} cy={cy + 1} r={4} fill="#a8455a" stroke={dark} strokeWidth={2} />;
    case "frown":
      return <path d={`M${cx - 8} ${cy + 4} Q${cx} ${cy - 5} ${cx + 8} ${cy + 4}`} fill="none" stroke={dark} strokeWidth={3} strokeLinecap="round" />;
    case "flat":
      return <line x1={cx - 6} y1={cy + 2} x2={cx + 6} y2={cy + 2} stroke={dark} strokeWidth={3} strokeLinecap="round" />;
    default: // neutral
      return <path d={`M${cx - 5} ${cy} Q${cx} ${cy + 4} ${cx + 5} ${cy}`} fill="none" stroke={dark} strokeWidth={2.6} strokeLinecap="round" />;
  }
}

function Accent({ kind }: { kind: Accent }) {
  switch (kind) {
    case "tear":
      return <path d="M30 56 q-3 7 0 10 q3 -3 0 -10 Z" fill="#7ec8f0" />;
    case "zzz":
      return <text x="74" y="30" fontSize="14" fill="#6b6b7a" fontFamily="sans-serif" fontWeight="700">z</text>;
    case "sparkle":
      return (
        <g fill="#ffd84d">
          <path d="M78 26 l2 5 5 2 -5 2 -2 5 -2 -5 -5 -2 5 -2 Z" />
          <path d="M20 34 l1.3 3.2 3.2 1.3 -3.2 1.3 -1.3 3.2 -1.3 -3.2 -3.2 -1.3 3.2 -1.3 Z" />
        </g>
      );
    case "think":
      return (
        <g fill="#9a9aa8">
          <circle cx="76" cy="40" r="2" />
          <circle cx="82" cy="34" r="2.6" />
          <circle cx="89" cy="27" r="3.2" />
        </g>
      );
    case "bang":
      return <text x="76" y="32" fontSize="18" fill="#e0556a" fontFamily="sans-serif" fontWeight="800">!</text>;
    case "wave":
      return (
        <g stroke="#2b2b35" strokeWidth={3} strokeLinecap="round" fill="none">
          <path d="M82 60 q6 -4 4 -12" />
        </g>
      );
    default:
      return null;
  }
}

function Silhouette({ style }: { style: Style }) {
  const { body, belly, outline, shape } = style;
  switch (shape) {
    case "roundsquare":
      return (
        <g>
          <rect x={18} y={26} width={64} height={58} rx={22} fill={body} stroke={outline} strokeWidth={2.5} />
          <ellipse cx={50} cy={64} rx={20} ry={14} fill={belly} />
        </g>
      );
    case "pixel":
      return (
        <g shapeRendering="crispEdges">
          <rect x={22} y={24} width={56} height={56} fill={body} stroke={outline} strokeWidth={3} />
          <rect x={32} y={58} width={36} height={16} fill={belly} />
        </g>
      );
    case "polaroid":
      return (
        <g>
          <rect x={16} y={18} width={68} height={74} rx={3} fill="#ffffff" stroke={outline} strokeWidth={2.5} />
          <rect x={23} y={25} width={54} height={48} fill={belly} />
          <rect x={23} y={76} width={54} height={10} fill="#f7f1e6" />
        </g>
      );
    default: // ellipse
      return (
        <g>
          <ellipse cx={50} cy={56} rx={34} ry={32} fill={body} stroke={outline} strokeWidth={2.5} />
          <ellipse cx={50} cy={64} rx={20} ry={16} fill={belly} />
        </g>
      );
  }
}

function EarTop({ style }: { style: Style }) {
  const { body, outline, ears } = style;
  switch (ears) {
    case "round":
      return (
        <g fill={body} stroke={outline} strokeWidth={2.5}>
          <circle cx={32} cy={28} r={9} />
          <circle cx={68} cy={28} r={9} />
        </g>
      );
    case "bow":
      return (
        <g fill="#ff5e93" stroke="#d14b78" strokeWidth={1.5}>
          <path d="M64 22 l10 -6 0 12 Z" />
          <path d="M76 22 l-10 -6 0 12 Z" transform="translate(8,0)" />
          <circle cx={74} cy={22} r={3} />
        </g>
      );
    case "antenna":
      return (
        <g>
          <line x1={50} y1={24} x2={50} y2={12} stroke={outline} strokeWidth={2.5} />
          <path d="M50 12 l4 -7 4 7 q-4 4 -8 0 Z" fill="#f1a7d0" stroke={outline} strokeWidth={1.2} />
        </g>
      );
    case "curl":
      return <path d="M50 26 q-2 -12 8 -12 q-6 4 -2 12" fill="none" stroke={outline} strokeWidth={2.5} strokeLinecap="round" />;
    case "pixel":
      return (
        <g fill={body} stroke={outline} strokeWidth={3} shapeRendering="crispEdges">
          <rect x={24} y={14} width={12} height={12} />
          <rect x={64} y={14} width={12} height={12} />
        </g>
      );
    default:
      return null;
  }
}

export function PetFace({ presetId, family, expression, size = 140, animate = true }: PetFaceProps) {
  const style = resolveStyle(presetId, family);
  const recipe = EXPR[expression] ?? EXPR.idle;
  // Only the live pet window animates; card thumbnails pass animate={false} so
  // N cards don't each run two infinite filtered transform animations.
  const animatable =
    animate && (expression === "idle" || expression === "smile" || expression === "peek");

  return (
    <svg
      className={`petface${animatable ? " petface--breathe" : ""}`}
      width={size}
      height={size}
      viewBox="0 0 100 100"
      aria-hidden
    >
      <g className="petface__body">
        <EarTop style={style} />
        <Silhouette style={style} />
        {/* cheeks */}
        <circle cx={30} cy={62} r={4} fill={style.cheek} opacity={0.7} />
        <circle cx={70} cy={62} r={4} fill={style.cheek} opacity={0.7} />
        <g className={animatable ? "petface__eyes" : undefined}>
          <Eyes kind={recipe.eye} />
        </g>
        <Mouth kind={recipe.mouth} />
      </g>
      <Accent kind={recipe.accent} />
    </svg>
  );
}
