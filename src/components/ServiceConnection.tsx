import type { ReactNode } from "react";
import Icon from "./Icon";
import ServiceIcon, { type ServiceName } from "./ServiceIcon";

export default function ServiceConnection({ service, name, description, status, connected, children, onOpen }: {
  service: ServiceName;
  name: string;
  description: string;
  status: string;
  connected: boolean;
  children: ReactNode;
  onOpen?: () => void;
}) {
  return (
    <details className="service-connection" onToggle={(event) => { if (event.currentTarget.open) onOpen?.(); }}>
      <summary className="service-summary">
        <ServiceIcon name={service} size={22} />
        <span className="service-description"><span className="service-name">{name}</span><span className="service-hint">{description}</span></span>
        <span className={`service-state ${connected ? "connected" : ""}`}>{status}</span>
        <Icon name="chevron-right" size={14} className="service-chevron" />
      </summary>
      <div className="service-configuration">{children}</div>
    </details>
  );
}
