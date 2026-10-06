import type { components } from '$lib/api/v3/openapi';
// Page model for the MusicBrainz settings. The v3 view is adapted into it in
// MusicBrainzAdapters.ts; the request bodies match the generated schemas.

export type MusicBrainzSourceMode = 'brainzmash' | 'official' | 'mirror' | 'community';

export interface BrainzMashPendingProposal {
	endpoint: string;
	access_revision: string;
	source_id: string;
	generation: number;
	disclosure_version: string;
	consented: boolean;
	verified: boolean;
}

export type BrainzMashActiveBinding = BrainzMashPendingProposal;

export interface MusicBrainzSettingsResponse {
	/** The source currently serving runtime MusicBrainz traffic. */
	source_mode: MusicBrainzSourceMode;
	/** The source selected in settings; while BrainzMash is active this may be pending. */
	selected_source_mode?: MusicBrainzSourceMode;
	/** Active endpoint; built-in BrainzMash responses use its pinned canonical endpoint. */
	api_url: string | null;
	rate_limit: number;
	concurrent_searches: number;
	community_acknowledged: boolean | null;
	/** Source identity is opaque; it scopes provider caches without exposing an endpoint. */
	source_id: string;
	generation: number;
	active_brainzmash?: BrainzMashActiveBinding | null;
	pending_brainzmash: BrainzMashPendingProposal | null;
	/** True when persisted source data was malformed and disabled at startup. */
	source_quarantined?: boolean;
	/** Safe, actionable migration explanation for administrators. */
	quarantine_reason?: string;
	/** Older backends may include this response-only warning during migration. */
	clamped_to_official_limits?: boolean;
}

export type MusicBrainzSettingsUpdate = components['schemas']['MusicBrainzSettingsUpdate'];

export type BrainzMashBinding = components['schemas']['MusicBrainzBindingRequest'];
