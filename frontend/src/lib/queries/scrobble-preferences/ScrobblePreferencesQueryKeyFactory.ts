import { ADMIN, key, userScope } from '../keys';

export const ScrobblePreferencesQueryKeyFactory = {
	get: (userId: string | null | undefined) => key(userScope(userId), 'me', 'scrobble-preferences'),
	// The admin queue, the same for every admin.
	personalMixApprovals: () => key(ADMIN, 'me', 'scrobble-preferences', 'personal-mix-approvals')
};
