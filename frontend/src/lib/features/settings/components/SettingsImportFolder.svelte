<script lang="ts">
	import Card from '$lib/ui/Card.svelte';
	import QueryBoundary from '$lib/ui/QueryBoundary.svelte';
	import SaveBar from '$lib/ui/SaveBar.svelte';
	import StatusChip from '$lib/ui/StatusChip.svelte';
	import TextField from '$lib/ui/TextField.svelte';
	import { createSettingsSection } from '$lib/ui/settingsSection.svelte';
	import {
		createSaveImportFolderSettingsMutation,
		getImportFolderSettingsQuery,
		getImportFolderStatusQuery
	} from '$lib/features/settings/queries/SectionSettingsQueries.svelte';
	import { IMPORT_FOLDER_ANCHOR, describeImportFolder } from './importFolderStatus';

	// Whether the settings drawn above this card have loaded: until then a
	// scroll to the card lands short of it.
	let { aboveReady = true }: { aboveReady?: boolean } = $props();

	const section = createSettingsSection(
		getImportFolderSettingsQuery(),
		createSaveImportFolderSettingsMutation()
	);
	const statusQuery = getImportFolderStatusQuery();

	// A link from the Downloads page lands here once the tab has drawn.
	let card = $state<HTMLElement | null>(null);
	let landed = false;
	$effect(() => {
		if (landed || !aboveReady || !card) return;
		landed = true;
		if (window.location.hash !== `#${IMPORT_FOLDER_ANCHOR}`) return;
		const target = card;
		requestAnimationFrame(() => target.scrollIntoView({ block: 'start' }));
	});

	const status = $derived(statusQuery.data ? describeImportFolder(statusQuery.data) : null);
	const overlapPath = $derived(statusQuery.data?.overlap_path ?? null);
	const overlapName = $derived(overlapPath?.split('/').filter(Boolean).pop() ?? null);
</script>

<section id={IMPORT_FOLDER_ANCHOR} bind:this={card} class="scroll-mt-20">
	<Card as="div" variant="bordered">
		<div class="flex flex-col gap-4">
			<div class="space-y-1">
				<div class="flex flex-wrap items-center justify-between gap-2">
					<h3 class="font-semibold">Import folder</h3>
					{#if status}
						<StatusChip tone={status.tone}>{status.label}</StatusChip>
					{/if}
				</div>
				<p class="text-xs text-muted">
					Bring in music you already have. Copy albums here and they're filed into your library,
					then removed from this folder.
				</p>
			</div>
			{#if overlapPath}
				<p class="text-xs text-warning">
					{overlapName} <span class="font-mono break-all">{overlapPath}</span>
				</p>
			{/if}
			<QueryBoundary query={section} what="the import folder">
				{#snippet children(draft)}
					<div class="flex flex-col gap-3">
						<TextField
							label="Folder"
							placeholder="/data/import"
							mono
							bind:value={draft.path}
							error={section.fieldError('path', 'Folder')}
						/>
						<SaveBar {...section.bar} />
					</div>
				{/snippet}
			</QueryBoundary>
		</div>
	</Card>
</section>
