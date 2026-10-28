import { beforeEach, describe, expect, it, vi } from 'vitest';

import type { LibraryManagementActivationSession } from './LibraryManagementPreviewTokens';
import {
	forgetLibraryManagementActivationSession,
	readLibraryManagementActivationSession,
	rememberLibraryManagementActivationSession
} from './LibraryManagementPreviewTokens';

const session: LibraryManagementActivationSession = {
	sourceRevision: 'settings-1',
	policyRevision: 'policy-1',
	draft: {
		schema_version: 1,
		preset_catalog_version: 1,
		profiles: [],
		default_profile_id: '',
		root_assignments: [],
		naming_scripts: [],
		tagging_scripts: [],
		undo_retention_days: 90,
		preview_retention_hours: 24,
		external_refresh: {
			enabled: false,
			plex_enabled: false,
			jellyfin_enabled: false,
			navidrome_enabled: false,
			retry_attempts: 3,
			retry_delay_seconds: 30
		}
	},
	activationDraft: {
		schema_version: 1,
		preset_catalog_version: 1,
		profiles: [],
		default_profile_id: '',
		root_assignments: [],
		naming_scripts: [],
		tagging_scripts: [],
		undo_retention_days: 90,
		preview_retention_hours: 24,
		external_refresh: {
			enabled: false,
			plex_enabled: false,
			jellyfin_enabled: false,
			navidrome_enabled: false,
			retry_attempts: 3,
			retry_delay_seconds: 30
		}
	},
	rootIds: ['root-1'],
	rootIndex: 0,
	jobId: 'preview-1',
	previewToken: 'secret-token',
	proofs: []
};

beforeEach(() => {
	vi.restoreAllMocks();
	sessionStorage.clear();
});

describe('Library Management activation session', () => {
	it('survives navigation in the current browser tab and remains user-scoped', () => {
		rememberLibraryManagementActivationSession('admin-1', session);

		expect(readLibraryManagementActivationSession('admin-1')).toEqual(session);
		expect(readLibraryManagementActivationSession('admin-2')).toBeNull();
	});

	it('forgets completed activation setup and rejects malformed state', () => {
		rememberLibraryManagementActivationSession('admin-1', session);
		forgetLibraryManagementActivationSession('admin-1');
		expect(readLibraryManagementActivationSession('admin-1')).toBeNull();

		sessionStorage.setItem(
			'droppedneedle:library-management:activation-session:admin-1',
			'{"rootIds":"not-an-array"}'
		);
		expect(readLibraryManagementActivationSession('admin-1')).toBeNull();
	});

	it('keeps settings usable when browser storage is unavailable', () => {
		vi.spyOn(Storage.prototype, 'setItem').mockImplementation(() => {
			throw new DOMException('Storage is unavailable', 'SecurityError');
		});
		expect(() => rememberLibraryManagementActivationSession('admin-1', session)).not.toThrow();

		vi.restoreAllMocks();
		vi.spyOn(Storage.prototype, 'getItem').mockImplementation(() => {
			throw new DOMException('Storage is unavailable', 'SecurityError');
		});
		expect(readLibraryManagementActivationSession('admin-1')).toBeNull();

		vi.restoreAllMocks();
		vi.spyOn(Storage.prototype, 'removeItem').mockImplementation(() => {
			throw new DOMException('Storage is unavailable', 'SecurityError');
		});
		expect(() => forgetLibraryManagementActivationSession('admin-1')).not.toThrow();
	});
});
