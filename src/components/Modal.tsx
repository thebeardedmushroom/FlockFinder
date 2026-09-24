import type { ReactNode } from "react";

interface Props {
  title: string;
  onClose?: () => void;
  footer?: ReactNode;
  width?: number;
  children: ReactNode;
}

/** Simple modal. Deliberately not a <form>: nothing inside submits on Enter. */
export default function Modal({ title, onClose, footer, width, children }: Props) {
  return (
    <div
      className="modal-backdrop"
      onMouseDown={(e) => {
        if (e.target === e.currentTarget && onClose) onClose();
      }}
    >
      <div className="modal" style={width ? { width } : undefined} role="dialog" aria-label={title}>
        <div className="panel-header">
          <span>{title}</span>
          {onClose && (
            <button className="close" onClick={onClose} aria-label="Close">
              ×
            </button>
          )}
        </div>
        <div className="panel-body">{children}</div>
        {footer && <div className="modal-footer">{footer}</div>}
      </div>
    </div>
  );
}
