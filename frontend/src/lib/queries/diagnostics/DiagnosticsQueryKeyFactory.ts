import { ADMIN, key } from '../keys';

/**
 * Admin-only runtime gauges. They describe the shared server process, the
 * same for every admin.
 */
export const DiagnosticsQueryKeyFactory = {
	all: () => key(ADMIN, 'diagnostics'),
	queueStats: () => key(ADMIN, 'diagnostics', 'queue-stats'),
	providerStats: () => key(ADMIN, 'diagnostics', 'provider-stats')
};
