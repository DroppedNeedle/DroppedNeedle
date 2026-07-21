/** localStorage key holding the last hydrated user id, so a page load as a
 *  different user (shared browser) can clear the persisted query cache (AMU-5). */
export const LAST_USER_ID_KEY = 'msr:last_user_id';

export interface AuthUser {
	id: string;
	display_name: string;
	role: 'admin' | 'trusted' | 'user';
	email: string | null;
	avatar_url: string | null;
	username: string | null;
	username_display: string | null;
	providers: string[];
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
			user = newUser;
		},

		clear() {
			user = null;
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
