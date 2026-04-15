import type { ReactNode } from "react";

import type { BannerTone, SignalTone } from "../dashboardModels";

export function Panel({
  eyebrow,
  title,
  detail,
  children,
  className,
}: {
  eyebrow: string;
  title: string;
  detail?: string;
  children: ReactNode;
  className?: string;
}) {
  const panelClassName = className ? `panel ${className}` : "panel";

  return (
    <section className={panelClassName}>
      <div className="panel__heading">
        <div>
          <p className="eyebrow">{eyebrow}</p>
          <h2>{title}</h2>
        </div>
        {detail ? <p className="panel__detail">{detail}</p> : null}
      </div>
      {children}
    </section>
  );
}

export function ControlCluster({
  eyebrow,
  title,
  detail,
  children,
}: {
  eyebrow: string;
  title: string;
  detail?: string;
  children: ReactNode;
}) {
  return (
    <section className="control-cluster">
      <div className="control-cluster__header">
        <div>
          <p className="eyebrow">{eyebrow}</p>
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
  return <span className={`pill pill--${tone}`}>{label}</span>;
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
