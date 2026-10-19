// The context window and the plan's limits, from the SDK's shapes to the
// protocol's (ADR 0014). The app reads them; this only translates.

import type {
  SDKControlGetContextUsageResponse,
  SDKControlGetUsageResponse,
  SDKRateLimitInfo,
} from "@anthropic-ai/claude-agent-sdk";

import type { ContextCategory, QuotaStatus, QuotaWindow, SessionQuota } from "./protocol.js";

export type ContextReport = Pick<SDKControlGetContextUsageResponse, "categories">;
export type UsageReport = Pick<SDKControlGetUsageResponse, "subscription_type" | "rate_limits_available" | "rate_limits">;

/** `getContextUsage` rows as the protocol's categories. */
export function contextCategories(r: ContextReport): ContextCategory[] {
  return r.categories.map((c) => ({
    name: c.name,
    tokens: Math.max(0, Math.round(c.tokens)),
    kind: c.kind,
  }));
}

/** An ISO time, or unix seconds or milliseconds, as unix milliseconds. */
function millis(at: string | number | null | undefined): number | undefined {
  if (at === null || at === undefined) return undefined;
  const ms = typeof at === "number" ? (at < 1e12 ? at * 1000 : at) : Date.parse(at);
  return Number.isFinite(ms) ? Math.round(ms) : undefined;
}

function limit(
  key: string,
  w: { utilization: number | null; resets_at: string | null } | null | undefined,
  label?: string,
): QuotaWindow[] {
  if (!w) return [];
  const resets = millis(w.resets_at);
  return [
    {
      key,
      ...(label ? { label } : {}),
      ...(w.utilization !== null ? { utilization: w.utilization } : {}),
      ...(resets !== undefined ? { resets_at: resets } : {}),
    },
  ];
}

/** The usage report (`/usage`): every plan window, replacing what the app knew.
 * `undefined` when limits apply but the report could not say (keep what is known). */
export function quotaFromUsage(r: UsageReport): SessionQuota | undefined {
  const plan = r.subscription_type ?? undefined;
  if (!r.rate_limits_available) return { complete: true, available: false, ...(plan ? { plan } : {}), windows: [] };
  const rl = r.rate_limits;
  if (!rl) return undefined;
  const windows = [
    ...limit("five_hour", rl.five_hour),
    ...limit("seven_day", rl.seven_day),
    ...limit("seven_day_opus", rl.seven_day_opus),
    ...limit("seven_day_sonnet", rl.seven_day_sonnet),
    ...(rl.model_scoped ?? []).flatMap((m) => limit(`model:${m.display_name}`, m, m.display_name)),
  ];
  const extra = rl.extra_usage;
  if (extra?.is_enabled && extra.utilization !== null) {
    windows.push({ key: "extra_usage", utilization: extra.utilization });
  }
  return { complete: true, available: true, ...(plan ? { plan } : {}), windows };
}

const STATUS: Record<SDKRateLimitInfo["status"], QuotaStatus> = {
  allowed: "allowed",
  allowed_warning: "warning",
  rejected: "rejected",
};

/** A `rate_limit_event`: the one window it is about. Its utilization is a
 * fraction and its reset unix seconds (the API's headers). */
export function quotaFromRateLimit(info: SDKRateLimitInfo): SessionQuota | undefined {
  if (!info.rateLimitType) return undefined;
  const key = info.rateLimitType === "overage" ? "extra_usage" : info.rateLimitType;
  const resets = millis(info.resetsAt);
  const window: QuotaWindow = {
    key,
    status: STATUS[info.status],
    ...(info.utilization !== undefined ? { utilization: Math.min(100, info.utilization * 100) } : {}),
    ...(resets !== undefined ? { resets_at: resets } : {}),
  };
  return { complete: false, available: true, windows: [window] };
}
