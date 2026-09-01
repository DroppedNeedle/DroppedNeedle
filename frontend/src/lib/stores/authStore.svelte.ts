import {
	resetMusicBrainzSourceScope,
	setMusicBrainzSourceScope
} from '$lib/queries/musicbrainz/sourceScope.svelte';
import type { MusicBrainzSourceMode } from '$lib/queries/musicbrainz/types';

/** localStorage key holding the last hydrated user id, so a page load as a
 * different user (shared browser) can clear the persisted query cache (AMU-5). */
export const LAST_USER_ID_KEY = 'msr:last_user_id';

export interface MusicBrainzSourceIdentity {
	source_mode: MusicBrainzSourceMode;
	source_id: string;
	generation: number;
}

export interface AuthUser {
	id: string;
	display_name: string;
	role: 'admin' | 'trusted' | 'user';
	email: string | null;
	avatar_url: string | null;
	username: string | null;
	username_display: string | null;
	providers: string[];
	musicbrainz_source?: MusicBrainzSourceIdentity | null;
}

function createAuthStore() {
	let user = $state<AuthUser | null>(null);
	let initialized = $state(false);
	let setupRequired = $state(false);

	return {
		get user() {
			return user;
		},
		get initialized() {
			return initialized;
		},
		get setupRequired() {
			return setupRequired;
		},
		get isAuthenticated() {
			return user !== null;
		},
		get isAdmin() {
			return user?.role === 'admin';
		},
		get isTrusted() {
			return user?.role === 'trusted' || user?.role === 'admin';
		},

		setUser(newUser: AuthUser) {
			const previousUserId = user?.id ?? null;
			user = newUser;
			if (newUser.musicbrainz_source) {
				setMusicBrainzSourceScope(newUser.musicbrainz_source, newUser.id);
			} else if (previousUserId !== newUser.id) {
				resetMusicBrainzSourceScope();
			}
		},

		clear() {
			user = null;
			resetMusicBrainzSourceScope();
		},

		markInitialized() {
			initialized = true;
		},

		setSetupRequired(required: boolean) {
			setupRequired = required;
		}
	};
}

export const authStore = createAuthStore();
