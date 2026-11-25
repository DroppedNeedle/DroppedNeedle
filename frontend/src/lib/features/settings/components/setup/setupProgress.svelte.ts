import { getDownloadClientConfigQuery } from '$lib/features/downloads/queries/DownloadClientQueries.svelte';
import {
	getDeemixConfigQuery,
	getNzbgetConfigQuery,
	getSabnzbdConfigQuery
} from '$lib/features/downloads/queries/DownloadClientsQueries.svelte';
import { useIntegrationStatus } from '$lib/features/home/queries/home.svelte';
import { getTargetLibrarySettingsQuery } from '$lib/features/library-management/queries/LibraryPolicyQueries.svelte';
import { getImportFolderSettingsQuery } from '$lib/features/settings/queries/SectionSettingsQueries.svelte';
import { getUsenetIndexerState } from '$lib/features/settings/queries/usenetIndexer.svelte';

/**
 * What is set up so far, read from the saved settings the setup checklist
 * reads. The guide stores nothing of its own: a step is done once its
 * settings are saved. Call while a component initialises.
 */
export function useSetupProgress() {
	const library = getTargetLibrarySettingsQuery();
	const importFolder = getImportFolderSettingsQuery();
	const slskd = getDownloadClientConfigQuery();
	const sabnzbd = getSabnzbdConfigQuery();
	const nzbget = getNzbgetConfigQuery();
	const deemix = getDeemixConfigQuery();
	const indexer = getUsenetIndexerState();
	const integration = useIntegrationStatus();

	const on = (config: { enabled: boolean; url: string } | undefined) =>
		config?.enabled === true && Boolean(config.url.trim());

	const progress = $derived.by(() => {
		const slskdSet = Boolean(slskd.data?.url && slskd.data?.api_key);
		const sabnzbdSet = on(sabnzbd.data);
		const nzbgetSet = on(nzbget.data);
		const deemixSet = on(deemix.data);
		return {
			library: (library.data?.library_roots.length ?? 0) > 0,
			acoustid: Boolean(library.data?.acoustid_api_key),
			importFolder: Boolean(importFolder.data?.path.trim()),
			slskd: slskdSet,
			sabnzbd: sabnzbdSet,
			nzbget: nzbgetSet,
			deemix: deemixSet,
			usenetClient: sabnzbdSet || nzbgetSet,
			usenetSearch: indexer.hasIndexer,
			// A Usenet client fetches nothing until something searches for it.
			downloads:
				integration.current.download_client ||
				slskdSet ||
				deemixSet ||
				((sabnzbdSet || nzbgetSet) && indexer.hasIndexer)
		};
	});

	return {
		get current() {
			return progress;
		}
	};
}
