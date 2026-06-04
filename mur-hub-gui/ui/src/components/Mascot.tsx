interface MascotProps { size?: number; floating?: boolean; className?: string; }

/** MuR starling. Hover raises brows + shifts pupils toward the viewer (house easter egg). */
export function Mascot({ size = 66, floating = false, className = "" }: MascotProps) {
  return (
    <div
      className={`bird ${floating ? "bird--float" : ""} ${className}`}
      style={{ width: size, height: size * 0.94 }}
      aria-hidden="true"
    >
      <div className="bird__feet" />
      <div className="bird__body" />
      <div className="bird__wing bird__wing--l" />
      <div className="bird__wing bird__wing--r" />
      <div className="bird__brow bird__brow--l" />
      <div className="bird__brow bird__brow--r" />
      <div className="bird__eye bird__eye--l" />
      <div className="bird__eye bird__eye--r" />
      <div className="bird__beak" />
    </div>
  );
}
