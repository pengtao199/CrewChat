/**
 * Usage API shapes — mirrors the backend `/api/crewchat/usage` response.
 *
 * Only consumption caused by CrewChat-driven CLI bodies is counted; the `llm`
 * built-in client and the user's own terminal usage are excluded.
 */

export type UsageBucket = {
  requests: number;
  input_tokens: number;
  output_tokens: number;
  cache_read_tokens: number;
  cache_creation_tokens: number;
  reasoning_tokens: number;
  cost_usd: number;
};

export type UsageSummary = {
  days: number;
  totals: UsageBucket;
  by_body: Array<UsageBucket & { body: string }>;
  by_model: Array<UsageBucket & { model: string; body: string }>;
  by_day: Array<UsageBucket & { date: string }>;
};
