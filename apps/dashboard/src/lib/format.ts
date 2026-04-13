export type DecimalLike = number | string | null | undefined;

const shortDateTime = new Intl.DateTimeFormat(undefined, {
  month: "short",
  day: "numeric",
  hour: "numeric",
  minute: "2-digit",
});

function asNumber(value: DecimalLike): number | null {
  if (value === null || value === undefined) {
    return null;
  }

  if (typeof value === "number") {
    return Number.isFinite(value) ? value : null;
  }

  const parsed = Number.parseFloat(value);
  return Number.isFinite(parsed) ? parsed : null;
}

export function formatDateTime(value: string | null | undefined): string {
  if (!value) {
    return "Unavailable";
  }

  const parsed = new Date(value);
  if (Number.isNaN(parsed.getTime())) {
    return value;
  }

  return shortDateTime.format(parsed);
}

export function formatMode(value: string): string {
  return value
    .replaceAll("_", " ")
    .replace(/\b\w/g, (character) => character.toUpperCase());
}

export function formatDecimal(value: DecimalLike): string {
  const parsed = asNumber(value);
  if (parsed === null) {
    return "Unavailable";
  }

  return parsed.toLocaleString(undefined, {
    minimumFractionDigits: 2,
    maximumFractionDigits: 2,
  });
}

export function formatSignedCurrency(value: DecimalLike): string {
  const parsed = asNumber(value);
  if (parsed === null) {
    return "Unavailable";
  }

  return parsed.toLocaleString(undefined, {
    style: "currency",
    currency: "USD",
    signDisplay: "always",
    minimumFractionDigits: 2,
    maximumFractionDigits: 2,
  });
}

export function formatCurrency(value: DecimalLike): string {
  const parsed = asNumber(value);
  if (parsed === null) {
    return "Unavailable";
  }

  return parsed.toLocaleString(undefined, {
    style: "currency",
    currency: "USD",
    minimumFractionDigits: 2,
    maximumFractionDigits: 2,
  });
}

export function formatInteger(value: number | null | undefined): string {
  if (value === null || value === undefined) {
    return "Unavailable";
  }

  return value.toLocaleString();
}

export function formatLatency(value: number | null | undefined): string {
  if (value === null || value === undefined) {
    return "Unavailable";
  }

  return `${value.toLocaleString()} ms`;
}
