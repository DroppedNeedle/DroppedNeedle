<script lang="ts">
	import type {
		FoundFolder,
		FoundService,
		SetupServiceKind
	} from '$lib/features/settings/queries/setupFound.svelte';
	import Card from '$lib/ui/Card.svelte';
	import SettingsDeemix from '../SettingsDeemix.svelte';
	import SettingsDownloadClient from '../SettingsDownloadClient.svelte';
	import SettingsIndexers from '../SettingsIndexers.svelte';
	import SettingsNzbget from '../SettingsNzbget.svelte';
	import SettingsSabnzbd from '../SettingsSabnzbd.svelte';
	import SetupSuggestion from './SetupSuggestion.svelte';
	import { SERVICE_NAMES } from './serviceNames';

	interface Props {
		services: FoundService[];
		folders: FoundFolder[];
		done: Partial<Record<SetupServiceKind, boolean>>;
		usenetClientSet: boolean;
		/** A download source is set up, whether or not it answered here. */
		sourceSet: boolean;
	}

	let { services, folders, done, usenetClientSet, sourceSet }: Props = $props();

	const CLIENTS: SetupServiceKind[] = ['slskd', 'sabnzbd', 'nzbget', 'deemix'];

	let open = $state<SetupServiceKind | null>(null);
	let dismissed = $state<SetupServiceKind[]>([]);

	const clients = $derived(services.filter((service) => CLIENTS.includes(service.kind)));
	const found = $derived(
		clients.filter((service) => service.outcome === 'found' && !dismissed.includes(service.kind))
	);
	const missing = $derived(
		clients.filter(
			(service) =>
				(service.outcome === 'not_found' || service.outcome === 'no_answer') &&
				done[service.kind] !== true
		)
	);
	const other = $derived(services.filter((service) => service.outcome === 'something_else'));
	const prowlarr = $derived(
		services.find((service) => service.kind === 'prowlarr' && service.outcome === 'found')
	);
	const downloadFolders = $derived(folders.filter((folder) => folder.role === 'downloads'));
	const setUp = $derived(CLIENTS.filter((kind) => done[kind] === true));
	const setUpText = $derived(
		setUp.length === 0
			? 'A download source is set up.'
			: `${setUp.map((kind) => SERVICE_NAMES[kind]).join(', ')} ${setUp.length === 1 ? 'is' : 'are'} set up.`
	);

	function note(service: FoundService): string | undefined {
		return service.note === 'sabnzbd_host_not_whitelisted'
			? "Add sabnzbd to SABnzbd's host whitelist (Config > Special), then press Test."
			: undefined;
	}
</script>

<div class="space-y-4">
	<p class="text-sm text-muted">What fetches the music you ask for. One is enough.</p>

	{#each found as service (service.kind)}
		<SetupSuggestion
			title={SERVICE_NAMES[service.kind]}
			detail={service.url}
			meta={service.version ? `Version ${service.version}` : undefined}
			note={note(service)}
			done={done[service.kind] === true}
			open={open === service.kind}
			onopen={() => (open = service.kind)}
			ondismiss={() => (dismissed = [...dismissed, service.kind])}
			onskip={() => (open = null)}
		>
			{#if service.kind === 'slskd'}
				<SettingsDownloadClient suggested={service.url} />
			{:else if service.kind === 'sabnzbd'}
				<SettingsSabnzbd suggested={service.url} />
			{:else if service.kind === 'nzbget'}
				<SettingsNzbget suggested={service.url} />
			{:else if service.kind === 'deemix'}
				<SettingsDeemix suggested={service.url} />
			{/if}
		</SetupSuggestion>
	{:else}
		<Card variant="bordered" padding="md">
			{#if sourceSet || setUp.length > 0}
				<p class="text-sm">{setUpText}</p>
				<p class="text-sm text-muted">Nothing else answered on this network.</p>
			{:else}
				<p class="text-sm">
					No download client answered on this network. Set one up under Settings > Download Client.
				</p>
			{/if}
		</Card>
	{/each}

	{#if usenetClientSet}
		<SettingsIndexers prowlarrSuggested={prowlarr?.url} />
	{/if}

	{#if missing.length > 0}
		<p class="text-sm text-muted">
			Not found: {missing.map((service) => SERVICE_NAMES[service.kind]).join(', ')}.
		</p>
	{/if}
	{#each other as service (service.kind)}
		<p class="text-sm text-muted">
			Something else answered at <span class="font-mono break-all">{service.url}</span>.
		</p>
	{/each}
	{#if downloadFolders.length > 0}
		<p class="text-sm text-muted">
			Download folders in this container: <span class="font-mono break-all"
				>{downloadFolders.map((folder) => folder.path).join(', ')}</span
			>.
		</p>
	{/if}
</div>
