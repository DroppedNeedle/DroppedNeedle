import { v3 } from '$lib/api/v3/endpoint';

// v3 session-management URLs, built through the typed registry: every
// template is a literal the contract-coverage gate verifies against the
// generated spec, and hooks import from here so a route rename touches this
// file only.
export const SESSIONS_ENDPOINTS = {
	list: () => v3('/api/v3/auth/sessions'),
	revoke: (sessionId: string) => v3('/api/v3/auth/sessions/{id}', { path: { id: sessionId } })
} as const;
