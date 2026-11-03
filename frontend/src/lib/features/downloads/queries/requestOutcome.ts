import { ALREADY_IN_LIBRARY_COPY } from '../components/acquisitionLabels';

export type AlbumRequestOutcome = 'dispatched' | 'submitted' | 'duplicate_active' | 'in_library';

/**
 * Maps a POST /requests/new payload onto the semantic tokens that
 * `requestStatusCopy` ($lib/utils/acquisitionLabels) renders. The backend cannot
 * express "already in the library" through its status alone - both a fresh accept
 * and the library hit return status "pending" - so the response message is the
 * distinguishing signal and raw statuses never reach the copy helper.
 */
export function albumRequestOutcome(response: {
	success: boolean;
	message?: string | null;
	status?: string | null;
}): AlbumRequestOutcome | null {
	if (!response.success) return null;
	if (response.message === ALREADY_IN_LIBRARY_COPY) return 'in_library';
	switch (response.status) {
		case 'awaiting_approval':
			return 'submitted';
		case 'queued':
		case 'downloading':
			return 'duplicate_active';
		default:
			return 'dispatched';
	}
}
