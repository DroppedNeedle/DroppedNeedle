<script lang="ts">
	import { Circle, CircleCheck } from 'lucide-svelte';
	import type {
		FoundService,
		SetupServiceKind
	} from '$lib/features/settings/queries/setupFound.svelte';
	import Button from '$lib/ui/Button.svelte';
	import { withBasePath } from '$lib/utils/basePath';
	import SetupSuggestion from './SetupSuggestion.svelte';
	import { SERVICE_NAMES, SERVICE_TABS } from './serviceNames';
	import type { SetupStep } from './steps';

	interface Progress {
		library: boolean;
		importFolder: boolean;
		acoustid: boolean;
		slskd: boolean;
		sabnzbd: boolean;
		nzbget: boolean;
		deemix: boolean;
		usenetClient: boolean;
		usenetSearch: boolean;
		downloads: boolean;
	}

	interface Props {
		services: FoundService[];
		progress: Progress;
		ongoto: (step: SetupStep) => void;
	}

	let { services, progress, ongoto }: Props = $props();

	const NEXT: SetupServiceKind[] = ['lidarr', 'jellyfin', 'navidrome'];
	// Loaded when opened: most installs never set these up from the guide.
	const FORMS = {
		lidarr: () => import('../SettingsLidarrImport.svelte'),
		jellyfin: () => import('../SettingsJellyfin.svelte'),
		navidrome: () => import('../SettingsNavidrome.svelte')
	} as const;

	function form(kind: SetupServiceKind) {
		return kind === 'lidarr' || kind === 'jellyfin' || kind === 'navidrome'
			? FORMS[kind]()
			: undefined;
	}

	let open = $state<SetupServiceKind | null>(null);

	const setUp = $derived(
		[
			progress.library && 'Library folder',
			progress.importFolder && 'Import folder',
			progress.acoustid && 'AcoustID key',
			progress.slskd && 'slskd',
			progress.sabnzbd && 'SABnzbd',
			progress.nzbget && 'NZBGet',
			progress.deemix && 'Deemix',
			progress.usenetClient && progress.usenetSearch && 'Usenet search'
		].filter((item): item is string => typeof item === 'string')
	);
	const skipped = $derived(
		(
			[
				[!progress.library, 'Library folder', 'music'],
				[!progress.importFolder, 'Import folder', 'music'],
				[!progress.downloads, 'A download source', 'downloads']
			] as const
		).filter(([unset]) => unset)
	);
	const next = $derived(
		services.filter((service) => NEXT.includes(service.kind) && service.outcome === 'found')
	);
</script>

<div class="space-y-6">
	<section class="space-y-2" aria-labelledby="setup-done">
		<h3 id="setup-done" class="font-semibold">Set up</h3>
		{#if setUp.length > 0}
			<ul class="space-y-1">
				{#each setUp as item (item)}
					<li class="flex items-center gap-2 text-sm">
						<CircleCheck class="size-4 shrink-0 text-success" aria-hidden="true" />
						{item}
					</li>
				{/each}
			</ul>
		{:else}
			<p class="text-sm text-muted">Nothing yet.</p>
		{/if}
	</section>

	{#if skipped.length > 0}
		<section class="space-y-2" aria-labelledby="setup-skipped">
			<h3 id="setup-skipped" class="font-semibold">Skipped</h3>
			<ul class="space-y-1">
				{#each skipped as [, label, step] (label)}
					<li class="flex flex-wrap items-center gap-2 text-sm">
						<Circle class="size-4 shrink-0 text-muted" aria-hidden="true" />
						{label}
						<Button variant="ghost" size="xs" onclick={() => ongoto(step)}>Set up</Button>
					</li>
				{/each}
			</ul>
		</section>
	{/if}

	{#if next.length > 0 || !progress.acoustid}
		<section class="space-y-3" aria-labelledby="setup-next">
			<h3 id="setup-next" class="font-semibold">Worth doing next</h3>
			{#each next as service (service.kind)}
				<SetupSuggestion
					title={SERVICE_NAMES[service.kind]}
					detail={service.url}
					meta={service.version ? `Version ${service.version}` : undefined}
					open={open === service.kind}
					onopen={() => (open = service.kind)}
					onskip={() => (open = null)}
				>
					{#await form(service.kind) then loaded}
						{#if loaded}
							<loaded.default suggested={service.url} />
						{/if}
					{/await}
				</SetupSuggestion>
				<p class="text-xs text-muted">
					Or set it up later in <a
						class="link"
						href={withBasePath(`/settings?tab=${SERVICE_TABS[service.kind]}`)}
						>Settings > {SERVICE_NAMES[service.kind]}</a
					>.
				</p>
			{/each}
			{#if !progress.acoustid}
				<p class="text-sm">
					Add an AcoustID key so music with no tags can be identified, in <a
						class="link"
						href={withBasePath('/settings?tab=library')}>Settings > Library</a
					>.
				</p>
			{/if}
		</section>
	{/if}

	<div class="flex justify-end">
		<Button variant="primary" href={withBasePath('/')}>Finish</Button>
	</div>
</div>
