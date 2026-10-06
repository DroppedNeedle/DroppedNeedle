import { v3 } from '$lib/api/v3/endpoint';

// v3 system URLs, built through the typed registry: every template is a
// literal the contract-coverage gate verifies against the generated spec.
export const SYSTEM_ENDPOINTS = {
	health: () => v3('/api/v3/system/health')
} as const;
