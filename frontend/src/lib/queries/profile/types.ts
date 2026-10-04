import type { components } from '$lib/api/v3/openapi';

/** Own profile: identity only. The v3 backend split the old fat profile -
 *  connected services live in the connections aggregate plus the remotes
 *  connection reads, library stats in the library/remotes stats reads - so
 *  the page composes those hooks instead of one query returning everything.
 *  `providers` stays the authoritative list used to choose change- vs
 *  set-password (D8). */
export type ProfileData = components['schemas']['ProfileResponse'];

/** One row of the profile's connected-services grid, mapped at runtime from
 *  the connections aggregate and the remotes connection reads. `url` is
 *  always null for media servers: v3 exposes server URLs only through
 *  admin settings reads, and members see this page too, so the grid renders
 *  those cards without an outbound link. */
export interface ProfileServiceConnection {
	name: string;
	enabled: boolean;
	username: string;
	url: string | null;
}

/** One card of the profile's libraries section, mapped at runtime from the
 *  per-source remotes stats reads (and the global library stats for local
 *  files). Sizes are null where v3 reports none; the section hides the size
 *  line then. */
export interface ProfileLibraryStats {
	source: string;
	total_tracks: number;
	total_albums: number;
	total_artists: number;
	total_size_bytes: number | null;
	total_size_human: string | null;
}

export interface DisplayNameUpdateVars {
	display_name: string;
}

export interface UsernameUpdateVars {
	username: string;
}

export interface EmailUpdateVars {
	email: string | null;
}

export interface ChangePasswordVars {
	current_password: string;
	new_password: string;
}

export interface SetPasswordVars {
	new_password: string;
}
