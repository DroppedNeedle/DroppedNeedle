import type { components } from '$lib/api/v3/openapi';
import type { BrainzMashPendingProposal, MusicBrainzSettingsResponse } from './types';

type View = components['schemas']['MusicBrainzSettingsView'];
type Proposal = components['schemas']['BrainzmashPendingProposal'];

// The settings page reads every field filled in; the one the view may leave
// out (`source_id`) is filled here.
function toProposal(proposal: Proposal): BrainzMashPendingProposal {
	return {
		endpoint: proposal.endpoint,
		access_revision: proposal.access_revision,
		source_id: proposal.source_id,
		generation: proposal.generation,
		disclosure_version: proposal.disclosure_version,
		consented: proposal.consented,
		verified: proposal.verified
	};
}

export function toMusicBrainzSettings(view: View): MusicBrainzSettingsResponse {
	return {
		source_mode: view.source_mode,
		selected_source_mode: view.selected_source_mode,
		api_url: view.api_url,
		rate_limit: view.rate_limit,
		community_acknowledged: view.community_acknowledged,
		source_id: view.source_id ?? '',
		generation: view.generation,
		active_brainzmash: view.active_brainzmash ? toProposal(view.active_brainzmash) : null,
		pending_brainzmash: view.pending_brainzmash ? toProposal(view.pending_brainzmash) : null,
		clamped_to_official_limits: view.clamped_to_official_limits
	};
}
