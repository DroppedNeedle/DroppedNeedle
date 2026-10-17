<!--
	The download that fills an edition's gaps: the buttons that start it,
	then the server's word on it and its live strip, or why it was refused
	with a way to try again.
-->
<script lang="ts" module>
	/** What became of asking for an edition's tracks. */
	export interface TracksOutcome {
		pending: boolean;
		message: string | null;
		taskId: string | null;
		error: string | null;
		replaceVersions: boolean;
	}
</script>

<script lang="ts">
	import AlbumDownloadStatus from '$lib/components/downloads/AlbumDownloadStatus.svelte';
	import { getAlbumDownloadsQuery } from '$lib/queries/downloads/DownloadQueries.svelte';
	import AsyncButton from '$lib/ui/AsyncButton.svelte';
	import { withBasePath } from '$lib/utils/basePath';
	import {
		NO_SOURCE_SENTENCE,
		REPLACE_ALSO_GETS_MISSING,
		getMissingLabel,
		replaceVersionsLabel
	} from './editionCopy';

	interface Props {
		/** The release group the album's downloads are filed under. */
		group: string;
		/** Edition tracks no file holds in any version. */
		missing: number;
		/** Edition tracks a file is another version of. */
		versions: number;
		sourceReady: boolean;
		outcome: TracksOutcome | null;
		onacquire: (replaceVersions: boolean) => Promise<void>;
	}

	let { group, missing, versions, sourceReady, outcome, onacquire }: Props = $props();

	const downloads = getAlbumDownloadsQuery(
		() => group,
		() => Boolean(outcome?.taskId)
	);
	const task = $derived(
		outcome?.taskId
			? (downloads.data?.items.find((item) => item.id === outcome.taskId) ?? null)
			: null
	);
</script>

{#snippet settingsLink()}
	<a class="link" href={withBasePath('/settings?tab=download-client')}>Download settings</a>
{/snippet}

<div class="flex w-full flex-col items-start gap-2 text-sm">
	{#if outcome?.pending}
		<p class="flex items-center gap-2">
			<span class="loading loading-spinner loading-xs" aria-hidden="true"></span>
			Starting the download...
		</p>
	{:else if outcome?.error}
		<p class="text-error" role="alert">{outcome.error}</p>
		<AsyncButton
			variant="outline"
			size="sm"
			onclick={() => onacquire(outcome?.replaceVersions ?? false)}>Try again</AsyncButton
		>
	{:else if outcome}
		{#if outcome.message}<p>{outcome.message}</p>{/if}
		{#if task}
			<div class="w-full max-w-xl"><AlbumDownloadStatus {task} /></div>
		{/if}
		{#if !sourceReady}{@render settingsLink()}{/if}
	{:else}
		<div class="flex flex-wrap gap-2">
			{#if missing > 0}
				<AsyncButton variant="outline" size="sm" onclick={() => onacquire(false)}
					>{getMissingLabel(missing)}</AsyncButton
				>
			{/if}
			{#if versions > 0}
				<AsyncButton variant="outline" size="sm" onclick={() => onacquire(true)}
					>{replaceVersionsLabel(versions)}</AsyncButton
				>
			{/if}
		</div>
		{#if missing > 0 && versions > 0}
			<p class="text-xs text-base-content/60">{REPLACE_ALSO_GETS_MISSING}</p>
		{/if}
		{#if !sourceReady}
			<p class="text-base-content/70">{NO_SOURCE_SENTENCE} {@render settingsLink()}</p>
		{/if}
	{/if}
</div>
