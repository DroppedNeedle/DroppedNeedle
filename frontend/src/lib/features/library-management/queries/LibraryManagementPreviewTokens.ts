import type { LibraryManagementActivationProof, LibraryManagementSettings } from './types';

const TOKEN_PREFIX = 'droppedneedle:library-management:preview-token:';
const ACTIVATION_SESSION_PREFIX = 'droppedneedle:library-management:activation-session:';

export interface LibraryManagementActivationSession {
	sourceRevision: string;
	policyRevision: string;
	draft: LibraryManagementSettings;
	activationDraft: LibraryManagementSettings;
	rootIds: string[];
	rootIndex: number;
	jobId: string | null;
	previewToken: string;
	proofs: LibraryManagementActivationProof[];
}

export function rememberLibraryManagementPreviewToken(jobId: string, token: string): void {
	if (typeof sessionStorage === 'undefined') return;
	sessionStorage.setItem(`${TOKEN_PREFIX}${jobId}`, token);
}

export function readLibraryManagementPreviewToken(jobId: string): string | null {
	if (typeof sessionStorage === 'undefined') return null;
	return sessionStorage.getItem(`${TOKEN_PREFIX}${jobId}`);
}

export function forgetLibraryManagementPreviewToken(jobId: string): void {
	if (typeof sessionStorage === 'undefined') return;
	sessionStorage.removeItem(`${TOKEN_PREFIX}${jobId}`);
}

export function rememberLibraryManagementActivationSession(
	userId: string,
	session: LibraryManagementActivationSession
): void {
	if (typeof sessionStorage === 'undefined') return;
	try {
		sessionStorage.setItem(`${ACTIVATION_SESSION_PREFIX}${userId}`, JSON.stringify(session));
	} catch {
		return;
	}
}

export function readLibraryManagementActivationSession(
	userId: string
): LibraryManagementActivationSession | null {
	if (typeof sessionStorage === 'undefined') return null;
	try {
		const serialized = sessionStorage.getItem(`${ACTIVATION_SESSION_PREFIX}${userId}`);
		if (!serialized) return null;
		const parsed = JSON.parse(serialized) as unknown;
		if (!isActivationSession(parsed)) {
			forgetLibraryManagementActivationSession(userId);
			return null;
		}
		return parsed;
	} catch {
		forgetLibraryManagementActivationSession(userId);
		return null;
	}
}

export function forgetLibraryManagementActivationSession(userId: string): void {
	if (typeof sessionStorage === 'undefined') return;
	try {
		sessionStorage.removeItem(`${ACTIVATION_SESSION_PREFIX}${userId}`);
	} catch {
		return;
	}
}

function isRecord(value: unknown): value is Record<string, unknown> {
	return typeof value === 'object' && value !== null && !Array.isArray(value);
}

function isActivationSession(value: unknown): value is LibraryManagementActivationSession {
	if (!isRecord(value) || !isRecord(value.draft) || !isRecord(value.activationDraft)) return false;
	if (!Array.isArray(value.rootIds) || !value.rootIds.every((item) => typeof item === 'string'))
		return false;
	if (!Array.isArray(value.proofs) || !value.proofs.every(isActivationProof)) return false;
	return (
		typeof value.sourceRevision === 'string' &&
		typeof value.policyRevision === 'string' &&
		typeof value.rootIndex === 'number' &&
		Number.isInteger(value.rootIndex) &&
		value.rootIndex >= 0 &&
		(value.jobId === null || typeof value.jobId === 'string') &&
		typeof value.previewToken === 'string' &&
		Array.isArray(value.draft.profiles) &&
		Array.isArray(value.draft.root_assignments) &&
		Array.isArray(value.activationDraft.profiles) &&
		Array.isArray(value.activationDraft.root_assignments)
	);
}

function isActivationProof(value: unknown): value is LibraryManagementActivationProof {
	return (
		isRecord(value) &&
		typeof value.root_id === 'string' &&
		typeof value.job_id === 'string' &&
		typeof value.preview_token === 'string'
	);
}
