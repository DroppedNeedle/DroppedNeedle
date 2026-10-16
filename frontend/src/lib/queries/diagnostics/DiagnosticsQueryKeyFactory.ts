/**
 * Admin-only runtime observability gauges. Nested under the
 * 'admin' parent segment so an admin-prefix sweep clears every gauge; there is
 * no userId dimension - gauges describe the shared backend process.
 */
export const DiagnosticsQueryKeyFactory = {
	prefix: ['admin', 'diagnostics'] as const,
	queueStats: () => [...DiagnosticsQueryKeyFactory.prefix, 'queue-stats'] as const,
	providerStats: () => [...DiagnosticsQueryKeyFactory.prefix, 'provider-stats'] as const
};
