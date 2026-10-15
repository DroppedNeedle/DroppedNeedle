<!--
	What happens after "Use this edition", live: the files being placed on
	it, then its tags and names, written by the app in a folder it manages
	by itself and previewed, then written, by the person in any other, and
	the download of its missing tracks.
-->
<script lang="ts">
	import { CircleCheck, Undo2 } from 'lucide-svelte';
	import AsyncButton from '$lib/ui/AsyncButton.svelte';
	import { authStore } from '$lib/stores/authStore.svelte';
	import { withBasePath } from '$lib/utils/basePath';
	import type {
		EditionChoice,
		EditionPlan,
		EditionStatus
	} from '$lib/queries/albums/EditionQueries.svelte';
	import EditionTracks, { type TracksOutcome } from './EditionTracks.svelte';
	import TagWritePreview from './TagWritePreview.svelte';
	import {
		filesPlacedSentence,
		placingSentence,
		tagsSentence,
		trulyMissing,
		type TagsMode
	} from './editionCopy';

	interface Props {
		name: string;
		status: EditionStatus | null;
		/** The album's own edition plan, once it is the chosen edition's. */
		plan: EditionPlan | null;
		choice: EditionChoice;
		/** The album is left unmanaged, by the person or by the app. */
		held: boolean;
		error: string | null;
		onundo: () => Promise<void>;
		/** The album copy, for writing its tags in a folder the app does not write by itself. */
		album: { userId: string | null | undefined; localId: string; rgMbid?: string };
		/** The release group the edition's tracks are fetched under. */
		group: string;
		/** The edition's gaps as planned when it was chosen. */
		gaps: { missing: number; versions: number };
		sourceReady: boolean;
		tracks: TracksOutcome | null;
		onacquire: (replaceVersions: boolean) => Promise<void>;
	}

	let {
		name,
		status,
		plan,
		choice,
		held,
		error,
		onundo,
		album,
		group,
		gaps,
		sourceReady,
		tracks,
		onacquire
	}: Props = $props();

	const missing = $derived(plan ? trulyMissing(plan) : gaps.missing);
	const versions = $derived(plan ? plan.other_version_count : gaps.versions);

	const placing = $derived(status?.placing ?? null);
	const files = $derived(
		placing
			? placingSentence(placing.attempts)
			: plan
				? filesPlacedSentence(plan.kept_count, plan.files.length - plan.kept_count)
				: filesPlacedSentence(choice.retag_files.length, choice.extra_track_ids.length)
	);
	const tags = $derived.by((): TagsMode => {
		if (held) return 'held';
		if (status?.files_by_itself) return status.writing || placing ? 'writing' : 'written';
		return 'manual';
	});
</script>

<section class="flex flex-col gap-4" aria-labelledby="edition-next-steps-title">
	<div class="flex flex-wrap items-start justify-between gap-3">
		<h3 id="edition-next-steps-title" class="flex items-start gap-2 text-base font-semibold">
			<CircleCheck class="mt-0.5 h-5 w-5 shrink-0 text-success" aria-hidden="true" />
			<span>This is your edition now: {name}</span>
		</h3>
		<AsyncButton class="btn btn-ghost btn-sm gap-1" onclick={onundo}>
			<Undo2 class="h-4 w-4" aria-hidden="true" /> Undo this change
		</AsyncButton>
	</div>
	{#if error}
		<p class="text-sm text-error" role="alert">{error}</p>
	{/if}
	<dl class="grid grid-cols-[4rem_minmax(0,1fr)] gap-x-4 gap-y-3 text-sm" aria-live="polite">
		<dt class="font-semibold text-base-content/60">Files</dt>
		<dd class="flex items-start gap-2">
			{#if placing}<span class="loading loading-spinner loading-xs mt-0.5" aria-hidden="true"
				></span>{/if}
			<span id="edition-files-sentence">{files}</span>
		</dd>
		<dt class="font-semibold text-base-content/60">Tags</dt>
		<dd class="flex items-start gap-2">
			{#if tags === 'writing'}<span
					class="loading loading-spinner loading-xs mt-0.5"
					aria-hidden="true"
				></span>{/if}
			{#if tags === 'manual' && choice.retag_files.length}
				<!-- A new choice starts its own preview. -->
				{#key choice}
					<TagWritePreview
						{album}
						files={choice.retag_files}
						placing={Boolean(placing)}
						placingSentenceId="edition-files-sentence"
					/>
				{/key}
			{:else if tags === 'written'}
				<span
					>Tags and file names are written. You can undo that from {#if authStore.isAdmin}<a
							class="link"
							href={withBasePath('/library/management/history')}>History</a
						>{:else}History{/if}.</span
				>
			{:else}
				<span>{tagsSentence(tags)}</span>
			{/if}
		</dd>
		{#if group && (tracks || missing > 0 || versions > 0)}
			<dt class="font-semibold text-base-content/60">Tracks</dt>
			<dd>
				<EditionTracks {group} {missing} {versions} {sourceReady} outcome={tracks} {onacquire} />
			</dd>
		{/if}
	</dl>
</section>
