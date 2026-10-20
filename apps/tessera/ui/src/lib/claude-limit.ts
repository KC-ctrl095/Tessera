// Preserve the public UTC reset in stored notices; display it in each
// device's own zone, including a date (weekly windows may cross midnight).
export function claudeLimitText(text: string): string {
  if (!/^(?:After (?:\d+ min|\d+ h \d+ min): )?(Your Claude plan's|You've used.*your Claude plan's)/.test(text)) return text;
  return text.replace(/\bIt resets at (\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}Z)\./, (whole, iso: string) => {
    const date = new Date(iso);
    if (!Number.isFinite(date.getTime())) return whole;
    const local = new Intl.DateTimeFormat(undefined, {
      month: "short", day: "numeric", hour: "numeric", minute: "2-digit", timeZoneName: "short",
    }).format(date);
    return `It resets at ${local}.`;
  });
}
