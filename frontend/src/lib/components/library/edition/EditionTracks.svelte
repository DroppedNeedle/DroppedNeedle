<!--
	The download that fills an edition's gaps: the buttons that start it,
	then, for as long as it runs or after it failed, each track it asked for
	with Stop and Retry. It reads the server's record of the fetch, so it
	shows the same after the sheet is closed and opened again.
-->
<script lang="ts" module>
	/** What became of asking for an edition's tracks, until the fetch shows. */
	export interface TracksOutcome {
		pending: boolean;
		message: string | null;
		error: string | null;
		replaceVersions: boolean;
	}
</script>

<script lang="ts">
	import { RotateCcw, Square } from 'lucide-svelte';
	import {
		editionFetchLive,
		getEditionFetchQuery,
		type EditionFetchTrackState,
		type EditionPlan
	} from '$lib/queries/albums/EditionQueries.svelte';
	import { cancelDownload } from '$lib/queries/downloads/DownloadMutations.svelte';
	import { authStore } from '$lib/stores/authStore.svelte';
	import AsyncButton from '$lib/ui/AsyncButton.svelte';
	import Button from '$lib/ui/Button.svelte';
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
		/** The library copy the tracks are fetched for. */
		localId: string;
		/** The edition's plan, which names its tracks. */
		plan: EditionPlan | null;
		/** Edition tracks no file holds in any version. */
		missing: number;
		/** Edition tracks a file is another version of. */
		versions: number;
		sourceReady: boolean;
		outcome: TracksOutcome | null;
		onacquire: (replaceVersions: boolean) => Promise<void>;
	}

	let { group, localId, plan, missing, versions, sourceReady, outcome, onacquire }: Props =
		$props();

	const fetchQuery = getEditionFetchQuery(
		() => authStore.user?.id,
		() => group,
		() => localId
	);
	// Shown while it runs, and after it failed or was stopped for this
	// edition. A finished one shows only to the person who just started it;
	// otherwise the album's own rows tell.
	const latest = $derived.by(() => {
		const current = fetchQuery.data?.latest ?? null;
		if (!current || (current.state === 'done' && !outcome)) return null;
		if (editionFetchLive(current)) return current;
		return !plan || current.release_mbid === plan.release_mbid ? current : null;
	});
	const titles = $derived(
		new Map((plan?.tracks ?? []).map((track) => [`${track.disc_number}-${track.position}`, track]))
	);
	const multiDisc = $derived(new Set(latest?.tracks.map((track) => track.disc)).size > 1);
	const counts = $derived.by(() => {
		const counts: Record<EditionFetchTrackState, number> = {
			waiting: 0,
			fetching: 0,
			landed: 0,
			failed: 0,
			held: 0
		};
		for (const track of latest?.tracks ?? []) counts[track.state] += 1;
		return counts;
	});
	const cancel = cancelDownload();

	const LABELS: Record<EditionFetchTrackState, string> = {
		waiting: 'Waiting',
		fetching: 'Fetching',
		landed: 'Added',
		failed: 'Failed',
		held: 'Held'
	};
	const BADGES: Record<EditionFetchTrackState, string> = {
		waiting: 'badge-ghost',
		fetching: 'badge-info badge-outline',
		landed: 'badge-success badge-outline',
		failed: 'badge-error badge-outline',
		held: 'badge-warning badge-outline'
	};

	function tracksWord(n: number): string {
		return `${n} ${n === 1 ? 'track' : 'tracks'}`;
	}

	const summary = $derived.by(() => {
		if (!latest) return '';
		const total = latest.tracks.length;
		const open = counts.waiting + counts.fetching;
		switch (latest.state) {
			case 'waiting':
				return total ? `Waiting to download ${tracksWord(total)}.` : 'Waiting to download.';
			case 'fetching':
				return total ? `Downloading ${tracksWord(open || total)}.` : 'Downloading the album.';
			case 'done':
				return total ? `${tracksWord(counts.landed)} added.` : 'Download finished.';
			case 'stopped':
				return 'Download stopped.';
			default:
				if (counts.failed) return `${tracksWord(counts.failed)} couldn't be downloaded.`;
				return counts.held ? '' : "The download couldn't finish.";
		}
	});
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
	{:else if latest}
		<div class="flex w-full flex-col gap-2" role="status" aria-live="polite">
			{#if summary}
				<p class="flex items-center gap-2">
					{#if editionFetchLive(latest)}
						<span class="loading loading-spinner loading-xs" aria-hidden="true"></span>
					{/if}
					{summary}
				</p>
			{/if}
			{#if counts.held}
				<p class="text-base-content/70">
					{tracksWord(counts.held)} held for you to check.
					<a class="link" href={withBasePath('/downloads')}>Review</a>
				</p>
			{/if}
			{#if latest.state === 'waiting' && !sourceReady}
				<p class="text-base-content/70">{NO_SOURCE_SENTENCE} {@render settingsLink()}</p>
			{/if}
			{#if latest.tracks.length}
				<ul
					class="w-full max-w-xl divide-y divide-base-content/10 rounded-box border border-base-content/10"
					aria-label="Tracks being fetched"
				>
					{#each latest.tracks as track (`${track.disc}-${track.position}`)}
						<li class="flex items-center gap-3 px-3 py-1.5">
							<span class="w-7 shrink-0 text-right text-base-content/60 tabular-nums">
								{multiDisc ? `${track.disc}-${track.position}` : track.position}
							</span>
							<span class="min-w-0 flex-1 truncate">
								{titles.get(`${track.disc}-${track.position}`)?.title ?? `Track ${track.position}`}
							</span>
							<span class="badge badge-sm shrink-0 {BADGES[track.state]}"
								>{LABELS[track.state]}</span
							>
						</li>
					{/each}
				</ul>
			{/if}
		</div>
		{#if latest.can_stop || latest.can_retry}
			<div class="flex flex-wrap gap-2">
				{#if latest.can_stop}
					<Button
						variant="outline"
						size="sm"
						icon={Square}
						disabled={cancel.isPending}
						onclick={() => cancel.mutate(latest.task_id)}>Stop</Button
					>
				{/if}
				{#if latest.can_retry}
					<AsyncButton
						variant="outline"
						size="sm"
						icon={RotateCcw}
						onclick={() => onacquire(latest.replace_versions)}>Retry</AsyncButton
					>
				{/if}
			</div>
		{/if}
	{:else if outcome}
		{#if outcome.message}<p>{outcome.message}</p>{/if}
		{#if !sourceReady}{@render settingsLink()}{/if}
	{:else}
		{#if missing > 0 || versions > 0}
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
		{/if}
		{#if !sourceReady}
			<p class="text-base-content/70">{NO_SOURCE_SENTENCE} {@render settingsLink()}</p>
		{/if}
	{/if}
</div>
