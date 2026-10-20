import { cloneElement, isValidElement, useId, type ReactElement, type ReactNode } from "react";

export default function SettingsRow({ label, desc, children }: {
  label: string;
  desc?: string;
  children: ReactNode;
}) {
  const descriptionId = useId();
  const control = isValidElement(children)
    ? cloneElement(children as ReactElement<{ "aria-label"?: string; "aria-describedby"?: string }>, {
        "aria-label": label,
        ...(desc ? { "aria-describedby": descriptionId } : {}),
      })
    : children;
  return (
    <div className="settings-row">
      <div className="settings-row-text">
        <div className="settings-row-label">{label}</div>
        {desc && <div className="settings-row-desc" id={descriptionId}>{desc}</div>}
      </div>
      <div className="settings-row-control">{control}</div>
    </div>
  );
}
