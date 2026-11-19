import { useId, useState, type ReactNode } from "react";
import { Icon } from "./ui.tsx";

/** Optional guidance (composer, contact card) is available by pointer, touch or keyboard. */
export function ComposerHelp({
  label,
  icon = "help",
  className,
  children,
}: {
  label: string;
  icon?: "help" | "info";
  className?: string;
  children: ReactNode;
}) {
  const id = useId();
  const [open, setOpen] = useState(false);
  return (
    <span
      className={className ? `composer-help ${className}` : "composer-help"}
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
            // Inside a modal dialog, Escape closes only the open hint first.
            if (open) event.preventDefault();
            setOpen(false);
            event.stopPropagation();
          }
        }}
      >
        <Icon name={icon} />
      </button>
      {open && (
        <span id={id} className="composer-help-content" role="tooltip">
          {children}
        </span>
      )}
    </span>
  );
}
