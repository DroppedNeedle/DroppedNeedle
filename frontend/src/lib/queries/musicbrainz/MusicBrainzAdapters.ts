import type { components } from '$lib/api/v3/openapi';
import type { BrainzMashPendingProposal, MusicBrainzSettingsResponse } from './types';

type View = components['schemas']['MusicBrainzSettingsView'];
type Proposal = components['schemas']['BrainzmashPendingProposal'];

// The contract marks every settings field optional (serde defaults on the
// server); the settings page needs them filled, so the defaults live here.
function toProposal(proposal: Proposal): BrainzMashPendingProposal {
	return {
		endpoint: proposal.endpoint ?? '',
		access_revision: proposal.access_revision ?? '',
		source_id: proposal.source_id ?? '',
		generation: proposal.generation ?? 0,
		disclosure_version: proposal.disclosure_version ?? '',
		consented: proposal.consented ?? false,
		verified: proposal.verified ?? false
	};
}

export function toMusicBrainzSettings(view: View): MusicBrainzSettingsResponse {
	return {
		source_mode: view.source_mode ?? 'official',
		selected_source_mode: view.selected_source_mode,
		api_url: view.api_url ?? null,
		rate_limit: view.rate_limit ?? 1,
		concurrent_searches: view.concurrent_searches ?? 1,
		community_acknowledged: view.community_acknowledged ?? null,
		source_id: view.source_id ?? '',
		generation: view.generation ?? 0,
		active_brainzmash: view.active_brainzmash ? toProposal(view.active_brainzmash) : null,
		pending_brainzmash: view.pending_brainzmash ? toProposal(view.pending_brainzmash) : null,
		clamped_to_official_limits: view.clamped_to_official_limits
	};
}
