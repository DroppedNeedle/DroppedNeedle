// Instance-wide admin settings, not per-user data: no userId segment
// (same as SystemQueryKeyFactory.health). The login/logout cache clears
// (AMU-5) still sweep them on user switch, and the queries only run for
// admins, so a persisted payload can never surface to a signed-out user.
export const ListenBrainzQueryKeyFactory = {
	prefix: ['settings'] as const,
	connection: () => [...ListenBrainzQueryKeyFactory.prefix, 'listenbrainz'] as const,
	scrobble: () => [...ListenBrainzQueryKeyFactory.prefix, 'scrobble'] as const
};
