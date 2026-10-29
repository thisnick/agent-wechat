import { useId, useState, type ReactNode } from "react";
import { Icon } from "./ui.tsx";

/** Optional composer guidance is available by pointer, touch or keyboard. */
export function ComposerHelp({
  label,
  children,
}: {
  label: string;
  children: ReactNode;
}) {
  const id = useId();
  const [open, setOpen] = useState(false);
  return (
    <span
      className="composer-help"
      onMouseEnter={() => setOpen(true)}
      onMouseLeave={() => setOpen(false)}
    >
      <button
        type="button"
        className="icon-btn"
        aria-label={label}
        aria-describedby={open ? id : undefined}
        onFocus={() => setOpen(true)}
        onBlur={() => setOpen(false)}
        onClick={() => setOpen(true)}
        onKeyDown={(event) => {
          if (event.key === "Escape") {
            setOpen(false);
            event.stopPropagation();
          }
        }}
      >
        <Icon name="help" />
      </button>
      {open && (
        <span id={id} className="composer-help-content" role="tooltip">
          {children}
        </span>
      )}
    </span>
  );
}
