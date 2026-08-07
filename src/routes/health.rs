/// Operations-only liveness probe. Plain text, not JSON — this endpoint must NOT follow the
/// ClientAPI "always 200 + `{error}`" convention, which is scoped to `/v1/puzzles/*` and
/// `/v1/public/*` only.
pub async fn healthz() -> &'static str {
    "ok"
}
