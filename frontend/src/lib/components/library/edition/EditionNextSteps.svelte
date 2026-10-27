<!--
	What happens after "Use this edition", live: the files being placed on
	it, then its tags and names, written by the app in a folder it manages
	by itself and previewed, then written, by the person in any other, and
	the download of its missing tracks.
-->
<script lang="ts">
	import { CircleCheck, Download, FolderInput, Tags, Undo2 } from 'lucide-svelte';
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
	const showTracks = $derived(Boolean(group && (tracks || missing > 0 || versions > 0)));
	const tags = $derived.by((): TagsMode => {
		if (held) return 'held';
		if (status?.files_by_itself) return status.writing || placing ? 'writing' : 'written';
		return 'manual';
	});
</script>

{#snippet stepIcon(Icon: typeof Tags, busy: boolean, joined: boolean)}
	{#if joined}
		<span
			class="absolute top-8 bottom-0 left-4 w-px -translate-x-1/2 bg-base-content/10"
			aria-hidden="true"
		></span>
	{/if}
	<span
		class="relative z-10 grid h-8 w-8 shrink-0 place-items-center rounded-full border border-base-content/10 bg-base-200 text-base-content/70"
		aria-hidden="true"
	>
		{#if busy}
			<span class="loading loading-spinner loading-xs"></span>
		{:else}
			<Icon class="h-4 w-4" />
		{/if}
	</span>
{/snippet}

<section class="flex flex-col gap-5" aria-labelledby="edition-next-steps-title">
	<div
		class="flex flex-wrap items-center justify-between gap-3 rounded-box border border-success/25 bg-success/10 px-4 py-3"
	>
		<h3 id="edition-next-steps-title" class="flex items-center gap-3 text-base font-semibold">
			<CircleCheck class="h-6 w-6 shrink-0 text-success" aria-hidden="true" />
			<span>This is your edition now: {name}</span>
		</h3>
		<AsyncButton variant="ghost" size="sm" icon={Undo2} onclick={onundo}
			>Undo this change</AsyncButton
		>
	</div>
	{#if error}
		<p class="text-sm text-error" role="alert">{error}</p>
	{/if}
	<ol class="flex flex-col text-sm" aria-live="polite">
		<li class="relative grid grid-cols-[2rem_minmax(0,1fr)] gap-x-3">
			{@render stepIcon(FolderInput, Boolean(placing), true)}
			<div class="flex min-w-0 flex-col gap-1 pb-5">
				<h4 class="text-xs font-semibold tracking-wide text-base-content/60 uppercase">Files</h4>
				<span id="edition-files-sentence">{files}</span>
			</div>
		</li>
		<li class="relative grid grid-cols-[2rem_minmax(0,1fr)] gap-x-3">
			{@render stepIcon(Tags, tags === 'writing', showTracks)}
			<div class="flex min-w-0 flex-col gap-1 {showTracks ? 'pb-5' : ''}">
				<h4 class="text-xs font-semibold tracking-wide text-base-content/60 uppercase">Tags</h4>
				{#if tags === 'manual' && choice.retag_files.length}
					<!-- A new choice starts its own preview. -->
					{#key choice}
						<TagWritePreview
							{album}
							edition={{
								releaseMbid: (status ?? choice.status).release_mbid ?? choice.release_mbid,
								identityRevision: (status ?? choice.status).identity_revision
							}}
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
			</div>
		</li>
		{#if showTracks}
			<li class="relative grid grid-cols-[2rem_minmax(0,1fr)] gap-x-3">
				{@render stepIcon(Download, Boolean(tracks?.pending), false)}
				<div class="flex min-w-0 flex-col gap-1">
					<h4 class="text-xs font-semibold tracking-wide text-base-content/60 uppercase">Tracks</h4>
					<EditionTracks
						{group}
						localId={album.localId}
						{plan}
						{missing}
						{versions}
						{sourceReady}
						outcome={tracks}
						{onacquire}
					/>
				</div>
			</li>
		{/if}
	</ol>
</section>
