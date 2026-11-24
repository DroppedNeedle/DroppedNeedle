import {
	getIndexersQuery,
	getSearchBackendQuery
} from '$lib/features/downloads/queries/IndexerQueries.svelte';
import { getProwlarrConfigQuery } from '$lib/features/downloads/queries/ProwlarrQueries.svelte';

// Whether Usenet search has an indexer to ask. The selected backend is what
// counts: with Prowlarr chosen, a Prowlarr address and key are the indexer;
// otherwise at least one Newznab indexer is. Presence only, as enabled flags
// and masked keys are the server's to judge.
export function getUsenetIndexerState() {
	const indexers = getIndexersQuery();
	const backend = getSearchBackendQuery();
	const prowlarr = getProwlarrConfigQuery();
	const viaProwlarr = () => backend.data?.backend === 'prowlarr';
	return {
		get loading() {
			return backend.isLoading || (viaProwlarr() ? prowlarr.isLoading : indexers.isLoading);
		},
		get hasIndexer() {
			return viaProwlarr()
				? Boolean(prowlarr.data?.url && prowlarr.data?.api_key)
				: (indexers.data?.length ?? 0) > 0;
		}
	};
}
