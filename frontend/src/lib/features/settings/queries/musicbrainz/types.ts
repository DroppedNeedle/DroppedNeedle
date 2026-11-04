import type { components } from '$lib/api/v3/openapi';
// Page model for the MusicBrainz settings. The v3 view is adapted into it in
// MusicBrainzAdapters.ts; the request bodies match the generated schemas.

export type MusicBrainzSourceMode = components['schemas']['MbSourceMode'];

export type BrainzMashPendingProposal = components['schemas']['BrainzmashPendingProposal'];

export type BrainzMashActiveBinding = components['schemas']['BrainzmashActiveBinding'];

export interface MusicBrainzSettingsResponse {
	/** The source currently serving runtime MusicBrainz traffic. */
	source_mode: MusicBrainzSourceMode;
	/** The source selected in settings; while BrainzMash is active this may be pending. */
	selected_source_mode?: MusicBrainzSourceMode;
	/** Active endpoint; built-in BrainzMash responses use its pinned canonical endpoint. */
	api_url: string | null;
	rate_limit: number;
	community_acknowledged: boolean | null;
	/** Source identity is opaque; it scopes provider caches without exposing an endpoint. */
	source_id: string;
	generation: number;
	active_brainzmash?: BrainzMashActiveBinding | null;
	pending_brainzmash: BrainzMashPendingProposal | null;
	/** Older backends may include this response-only warning during migration. */
	clamped_to_official_limits?: boolean;
}

export type MusicBrainzSettingsUpdate = components['schemas']['MusicBrainzSettingsUpdate'];

export type BrainzMashBinding = components['schemas']['MusicBrainzBindingRequest'];
