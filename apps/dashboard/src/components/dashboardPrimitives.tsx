import type { ReactNode } from "react";

import type { BannerTone, SignalTone } from "../dashboardModels";

export function Panel({
  eyebrow,
  title,
  detail,
  children,
  className,
  hideHeading = false,
}: {
  eyebrow: string;
  title: string;
  detail?: string;
  children: ReactNode;
  className?: string;
  hideHeading?: boolean;
}) {
  const panelClassName = className ? `panel ${className}` : "panel";

  return (
    <section className={panelClassName}>
      {hideHeading ? null : (
        <div className="panel__heading">
          <div>
            <p className="eyebrow">{eyebrow}</p>
            <h2>{title}</h2>
          </div>
          {detail ? <p className="panel__detail">{detail}</p> : null}
        </div>
      )}
      {children}
    </section>
  );
}

export function ControlCluster({
  eyebrow,
  title,
  detail,
  children,
  className,
  compact = false,
  hideEyebrow = false,
}: {
  eyebrow: string;
  title: string;
  detail?: string;
  children: ReactNode;
  className?: string;
  compact?: boolean;
  hideEyebrow?: boolean;
}) {
  const clusterClassName = className
    ? `control-cluster ${className}${compact ? " control-cluster--compact" : ""}`
    : `control-cluster${compact ? " control-cluster--compact" : ""}`;
  const headerClassName = compact
    ? "control-cluster__header control-cluster__header--compact"
    : "control-cluster__header";

  return (
    <section className={clusterClassName}>
      <div className={headerClassName}>
        <div>
          {hideEyebrow ? null : <p className="eyebrow">{eyebrow}</p>}
          <h3>{title}</h3>
        </div>
        {detail ? <p className="control-cluster__detail">{detail}</p> : null}
      </div>
      {children}
    </section>
  );
}

export function SectionBlock({
  title,
  note,
  children,
  className,
}: {
  title: string;
  note?: string;
  children: ReactNode;
  className?: string;
}) {
  const sectionClassName = className ? `section-block ${className}` : "section-block";

  return (
    <section className={sectionClassName}>
      <div className="section-block__header">
        <p className="control-card__title">{title}</p>
        {note ? <p className="section-block__note">{note}</p> : null}
      </div>
      {children}
    </section>
  );
}

export function Pill({
  label,
  tone,
}: {
  label: string;
  tone: BannerTone;
}) {
  return (
    <span className={`pill pill--${tone}`} title={label}>
      <span className="pill__label">{label}</span>
    </span>
  );
}

export function Metric({ label, value }: { label: string; value: string }) {
  return (
    <div className="metric">
      <span>{label}</span>
      <strong>{value}</strong>
    </div>
  );
}

export function SignalTile({
  label,
  value,
  detail,
  tone,
}: {
  label: string;
  value: string;
  detail?: string;
  tone: SignalTone;
}) {
  return (
    <div className={`signal-tile signal-tile--${tone}`}>
      <span className="signal-tile__label">{label}</span>
      <strong>{value}</strong>
      {detail ? <p className="signal-tile__detail">{detail}</p> : null}
    </div>
  );
}

export function MiniMetric({ label, value }: { label: string; value: string }) {
  return (
    <div className="mini-metric">
      <span>{label}</span>
      <strong>{value}</strong>
    </div>
  );
}

export function Definition({ label, value }: { label: string; value: string }) {
  return (
    <>
      <dt>{label}</dt>
      <dd>{value}</dd>
    </>
  );
}
