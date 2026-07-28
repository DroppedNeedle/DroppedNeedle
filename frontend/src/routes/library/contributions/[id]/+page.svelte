<script lang="ts">
	import { resolve } from '$app/paths';
	import { goto } from '$app/navigation';
	import {
		AlertTriangle,
		ArrowLeft,
		Check,
		CircleDot,
		Disc3,
		FilePenLine,
		FolderHeart,
		History,
		ListMusic,
		LoaderCircle,
		RotateCcw,
		Save,
		Trash2
	} from 'lucide-svelte';
	import type { PageData } from './$types';
	import type { ReleaseDraft, ReleaseTextField } from '$lib/types';
	import ContributionDiscogsSource from '$lib/components/library/ContributionDiscogsSource.svelte';
	import ContributionDiscogsComparison from '$lib/components/library/ContributionDiscogsComparison.svelte';
	import ContributionMusicBrainzReview from '$lib/components/library/ContributionMusicBrainzReview.svelte';
	import { authStore } from '$lib/stores/authStore.svelte';
	import { getLibraryContributionQuery } from '$lib/queries/libraryContributions/LibraryContributionQueries.svelte';
	import {
		cancelLibraryContributionMutation,
		rebuildLibraryContributionMutation,
		updateLibraryContributionMutation
	} from '$lib/queries/libraryContributions/LibraryContributionMutations.svelte';

	let { data }: { data: PageData } = $props();
	const contributionQuery = getLibraryContributionQuery(() => data.contributionId);
	const saveMutation = updateLibraryContributionMutation();
	const rebuildMutation = rebuildLibraryContributionMutation();
	const cancelMutation = cancelLibraryContributionMutation();
	const contribution = $derived(contributionQuery.data);
	let draft = $state<ReleaseDraft | null>(null);
	let loadedDraft = $state<ReleaseDraft | null>(null);
	let loadedRevision = $state(0);
	let cancelDialog = $state<HTMLDialogElement | null>(null);

	const mediaCount = $derived(contribution?.local_snapshot.media.length ?? 0);
	const trackCount = $derived(
		contribution?.local_snapshot.media.reduce((total, medium) => total + medium.tracks.length, 0) ??
			0
	);
	const canEdit = $derived(
		Boolean(
			authStore.isTrusted &&
			contribution &&
			['draft', 'ready', 'needs_review'].includes(contribution.state) &&
			contribution.next_actions.includes('edit_draft')
		)
	);
	const draftIsDirty = $derived(
		Boolean(draft && loadedDraft && JSON.stringify(draft) !== JSON.stringify(loadedDraft))
	);
	const canUseProviderActions = $derived(authStore.isTrusted && !draftIsDirty);
	const capturedAt = $derived(
		contribution ? new Date(contribution.local_snapshot.captured_at * 1000).toLocaleString() : ''
	);

	$effect(() => {
		if (
			contribution &&
			contribution.row_revision !== loadedRevision &&
			(!draftIsDirty || JSON.stringify(draft) === JSON.stringify(contribution.draft))
		) {
			draft = structuredClone(contribution.draft);
			loadedDraft = structuredClone(contribution.draft);
			loadedRevision = contribution.row_revision;
		}
	});

	function changeField(field: ReleaseTextField, value: string): void {
		field.value = value.trim() ? value : null;
		field.source = 'entered_here';
	}

	function useReleaseValue(
		field:
			| 'title'
			| 'artist_credit'
			| 'release_date'
			| 'country'
			| 'label'
			| 'catalogue_number'
			| 'barcode',
		value: string | null,
		source: 'local' | 'discogs'
	): void {
		if (!draft) return;
		draft[field].value = value;
		draft[field].source = source;
	}

	function useMediumValue(
		position: number,
		field: 'title' | 'format',
		value: string | null,
		source: 'local' | 'discogs'
	): void {
		const medium = draft?.media.find((item) => item.position === position);
		if (!medium) return;
		medium[field].value = value;
		medium[field].source = source;
	}

	function useTrackValues(
		localTrackId: string,
		title: string | null,
		artistName: string | null,
		source: 'local' | 'discogs'
	): void {
		const track = draft?.media
			.flatMap((medium) => medium.tracks)
			.find((item) => item.local_track_id === localTrackId);
		if (!track) return;
		track.title.value = title;
		track.title.source = source;
		track.artist_name.value = artistName;
		track.artist_name.source = source;
	}

	function discardLocalEdits(): void {
		if (!contribution) return;
		draft = structuredClone(contribution.draft);
		loadedDraft = structuredClone(contribution.draft);
		loadedRevision = contribution.row_revision;
	}

	function save(): void {
		if (!contribution || !draft || saveMutation.isPending) return;
		saveMutation.mutate({
			contributionId: contribution.id,
			expectedRowRevision: loadedRevision,
			draft: $state.snapshot(draft)
		});
	}

	function rebuild(): void {
		if (!contribution || rebuildMutation.isPending) return;
		rebuildMutation.mutate({
			contributionId: contribution.id,
			expectedRowRevision: contribution.row_revision
		});
	}

	function cancel(): void {
		if (!contribution || cancelMutation.isPending) return;
		cancelMutation.mutate(
			{
				contributionId: contribution.id,
				expectedRowRevision: contribution.row_revision
			},
			{ onSuccess: () => cancelDialog?.close() }
		);
	}

	const stages = $derived([
		{ label: 'Sources', detail: 'Local metadata', available: true },
		{
			label: 'Compare',
			detail: 'Review and correct',
			available: Boolean(contribution?.discogs_source?.release)
		},
		{
			label: 'Duplicates',
			detail: 'MusicBrainz check',
			available: Boolean(contribution?.duplicate_result)
		},
		{
			label: 'Review',
			detail: 'Release preview',
			available: Boolean(
				contribution?.duplicate_result &&
				!contribution.duplicate_result.candidates.some((candidate) => candidate.exact)
			)
		},
		{
			label: 'MusicBrainz',
			detail: 'Submit and verify',
			available: Boolean(
				contribution && ['seeded', 'verifying', 'linked'].includes(contribution.state)
			)
		}
	] as const);
</script>

<svelte:head><title>MusicBrainz contribution · DroppedNeedle</title></svelte:head>

<main class="mx-auto w-full max-w-7xl px-4 py-6 sm:px-6 lg:px-8 lg:py-10">
	{#if contributionQuery.isLoading}
		<div class="grid gap-6 lg:grid-cols-[15rem_minmax(0,1fr)]" role="status" aria-busy="true">
			<div class="space-y-3">
				<div class="skeleton h-8 w-36"></div>
				<div class="skeleton h-72 w-full rounded-box"></div>
			</div>
			<div class="space-y-5">
				<div class="skeleton h-28 w-full rounded-box"></div>
				<div class="skeleton h-64 w-full rounded-box"></div>
				<div class="skeleton h-80 w-full rounded-box"></div>
			</div>
		</div>
	{:else if contributionQuery.isError || !contribution}
		<div class="mx-auto max-w-xl py-20">
			<div class="alert alert-error" role="alert">
				<AlertTriangle class="h-5 w-5" />
				<div>
					<h1 class="font-bold">Couldn't open this contribution</h1>
					<p class="text-sm">It may have been removed, or the album is no longer in the library.</p>
				</div>
				<button class="btn btn-sm" onclick={() => contributionQuery.refetch()}>Retry</button>
			</div>
		</div>
	{:else}
		<div class="mb-6 flex flex-wrap items-center justify-between gap-3">
			<button
				class="btn btn-ghost btn-sm gap-2"
				onclick={() => goto(resolve(`/album/${contribution.local_album_id}`))}
			>
				<ArrowLeft class="h-4 w-4" /> Back to album
			</button>
			<span class="badge badge-outline gap-1.5 capitalize">
				<CircleDot class="h-3.5 w-3.5" />
				{contribution.state.replace('_', ' ')}
			</span>
		</div>

		<div class="grid gap-6 lg:grid-cols-[15rem_minmax(0,1fr)] lg:items-start">
			<aside class="lg:sticky lg:top-5" aria-label="Contribution progress">
				<div class="rounded-box border border-base-content/10 bg-base-200/50 p-4">
					<p class="text-xs font-bold uppercase tracking-[0.18em] text-base-content/45">
						Contribution steps
					</p>
					<ol class="mt-4 space-y-1">
						{#each stages as stage, index (stage.label)}
							<li
								class="grid grid-cols-[1.5rem_1fr] gap-2 rounded-box px-2 py-2.5 {stage.available
									? 'bg-base-100 text-base-content'
									: 'text-base-content/40'}"
							>
								<span
									class="grid h-6 w-6 place-items-center rounded-full border text-xs font-bold {index ===
									0
										? 'border-primary bg-primary text-primary-content'
										: 'border-base-content/20'}">{index + 1}</span
								>
								<span>
									<span class="block text-sm font-semibold">{stage.label}</span>
									<span class="block text-xs opacity-65">{stage.detail}</span>
								</span>
							</li>
						{/each}
					</ol>
				</div>
			</aside>

			<div class="min-w-0 space-y-6">
				<header class="overflow-hidden rounded-box border border-base-content/10 bg-base-200/45">
					<div class="border-b border-base-content/10 px-5 py-5 sm:px-7">
						<p class="text-sm font-semibold text-primary">Prepare a MusicBrainz release</p>
						<h1 class="mt-1 text-2xl font-black tracking-tight sm:text-4xl">
							{contribution.local_snapshot.title || 'Untitled local album'}
						</h1>
						<p class="mt-2 max-w-2xl text-sm text-base-content/60">
							Check what DroppedNeedle read from your files. Your corrections are saved to this
							draft only and won't change the album or its files.
						</p>
					</div>
					<div class="grid gap-px bg-base-content/10 sm:grid-cols-3">
						<div class="bg-base-100/75 px-5 py-3">
							<span class="block text-xs text-base-content/45">Artist credit</span>
							<span class="block truncate text-sm font-semibold">
								{contribution.local_snapshot.album_artist_name || 'Missing'}
							</span>
						</div>
						<div class="bg-base-100/75 px-5 py-3">
							<span class="block text-xs text-base-content/45">Media and tracks</span>
							<span class="block text-sm font-semibold"
								>{mediaCount}
								{mediaCount === 1 ? 'medium' : 'media'} · {trackCount}
								{trackCount === 1 ? 'track' : 'tracks'}</span
							>
						</div>
						<div class="bg-base-100/75 px-5 py-3">
							<span class="block text-xs text-base-content/45">Local snapshot</span>
							<span class="block truncate text-sm font-semibold">{capturedAt}</span>
						</div>
					</div>
				</header>

				{#if contribution.state === 'stale'}
					<section
						class="rounded-box border border-warning/35 bg-warning/10 p-5"
						aria-labelledby="stale-title"
					>
						<div class="flex items-start gap-3">
							<History class="mt-0.5 h-5 w-5 shrink-0 text-warning" />
							<div class="min-w-0 flex-1">
								<h2 id="stale-title" class="font-bold">The local album changed</h2>
								<p class="mt-1 text-sm text-base-content/65">
									Rebuild the draft before continuing. The previous version stays in the audit
									history.
								</p>
								{#if contribution.next_actions.includes('rebuild')}
									<button
										class="btn btn-warning btn-sm mt-4 gap-2"
										disabled={rebuildMutation.isPending}
										onclick={rebuild}
									>
										{#if rebuildMutation.isPending}<LoaderCircle
												class="h-4 w-4 animate-spin"
											/>{:else}<RotateCcw class="h-4 w-4" />{/if}
										Rebuild draft
									</button>
								{/if}
							</div>
						</div>
					</section>
				{:else if contribution.state === 'cancelled'}
					<section class="rounded-box border border-base-content/10 bg-base-200/45 p-6 text-center">
						<Trash2 class="mx-auto h-8 w-8 text-base-content/30" />
						<h2 class="mt-3 text-lg font-bold">This contribution was cancelled</h2>
						<p class="mt-1 text-sm text-base-content/55">
							You can start a fresh draft from the album page.
						</p>
					</section>
				{:else if draft}
					{@const currentDraft = draft}
					{#if draftIsDirty}
						<div class="alert alert-info" role="status">
							<FilePenLine class="h-5 w-5" />
							<div class="min-w-0 flex-1">
								<p class="font-semibold">Save this draft before checking provider data</p>
								<p class="text-sm">Discogs and MusicBrainz actions use the last saved version.</p>
							</div>
							<button class="btn btn-ghost btn-sm" onclick={discardLocalEdits}>Discard edits</button
							>
						</div>
					{/if}
					<section
						class="rounded-box border border-base-content/10 bg-base-100"
						aria-labelledby="local-source-title"
					>
						<div
							class="flex flex-wrap items-start justify-between gap-3 border-b border-base-content/10 px-5 py-4 sm:px-6"
						>
							<div class="flex items-start gap-3">
								<div
									class="grid h-10 w-10 place-items-center rounded-box bg-primary/10 text-primary"
								>
									<FolderHeart class="h-5 w-5" />
								</div>
								<div>
									<h2 id="local-source-title" class="font-bold">Local metadata</h2>
									<p class="text-sm text-base-content/55">Read from the indexed audio files</p>
								</div>
							</div>
							<span class="badge badge-success badge-outline gap-1"
								><Check class="h-3.5 w-3.5" /> Current</span
							>
						</div>
						<div class="grid gap-4 p-5 sm:grid-cols-2 sm:p-6">
							<label class="form-control sm:col-span-2">
								<span class="label"
									><span class="label-text font-semibold">Release title</span></span
								>
								<input
									class="input input-bordered w-full"
									value={currentDraft.title.value ?? ''}
									disabled={!canEdit}
									oninput={(event) => changeField(currentDraft.title, event.currentTarget.value)}
								/>
								<span class="label"
									><span class="label-text-alt text-base-content/45"
										>{currentDraft.title.source.replace('_', ' ')}</span
									></span
								>
							</label>
							<label class="form-control">
								<span class="label"
									><span class="label-text font-semibold">Artist credit</span></span
								>
								<input
									class="input input-bordered w-full"
									value={currentDraft.artist_credit.value ?? ''}
									disabled={!canEdit}
									oninput={(event) =>
										changeField(currentDraft.artist_credit, event.currentTarget.value)}
								/>
							</label>
							<label class="form-control">
								<span class="label"><span class="label-text font-semibold">Release date</span></span
								>
								<input
									class="input input-bordered w-full"
									value={currentDraft.release_date.value ?? ''}
									disabled={!canEdit}
									placeholder="YYYY, YYYY-MM, or YYYY-MM-DD"
									oninput={(event) =>
										changeField(currentDraft.release_date, event.currentTarget.value)}
								/>
							</label>
							<label class="form-control">
								<span class="label"><span class="label-text font-semibold">Country</span></span>
								<input
									class="input input-bordered w-full"
									value={currentDraft.country.value ?? ''}
									disabled={!canEdit}
									oninput={(event) => changeField(currentDraft.country, event.currentTarget.value)}
								/>
							</label>
							<label class="form-control">
								<span class="label"><span class="label-text font-semibold">Label</span></span>
								<input
									class="input input-bordered w-full"
									value={currentDraft.label.value ?? ''}
									disabled={!canEdit}
									oninput={(event) => changeField(currentDraft.label, event.currentTarget.value)}
								/>
							</label>
							<label class="form-control">
								<span class="label"
									><span class="label-text font-semibold">Catalogue number</span></span
								>
								<input
									class="input input-bordered w-full"
									value={currentDraft.catalogue_number.value ?? ''}
									disabled={!canEdit}
									oninput={(event) =>
										changeField(currentDraft.catalogue_number, event.currentTarget.value)}
								/>
							</label>
							<label class="form-control">
								<span class="label"><span class="label-text font-semibold">Barcode</span></span>
								<input
									class="input input-bordered w-full"
									value={currentDraft.barcode.value ?? ''}
									disabled={!canEdit}
									inputmode="numeric"
									oninput={(event) => changeField(currentDraft.barcode, event.currentTarget.value)}
								/>
							</label>
							<label class="form-control">
								<span class="label"><span class="label-text font-semibold">Packaging</span></span>
								<input
									class="input input-bordered w-full"
									value={currentDraft.packaging.value ?? ''}
									disabled={!canEdit}
									oninput={(event) =>
										changeField(currentDraft.packaging, event.currentTarget.value)}
								/>
							</label>
						</div>
					</section>

					<ContributionDiscogsSource {contribution} canMutate={canUseProviderActions} />

					<ContributionDiscogsComparison
						{contribution}
						draft={currentDraft}
						canMutate={canUseProviderActions}
						onuse={useReleaseValue}
						onusemedium={useMediumValue}
						onusetrack={useTrackValues}
					/>

					<section
						class="rounded-box border border-base-content/10 bg-base-100"
						aria-labelledby="tracklist-title"
					>
						<div class="flex items-center gap-3 border-b border-base-content/10 px-5 py-4 sm:px-6">
							<ListMusic class="h-5 w-5 text-primary" />
							<div>
								<h2 id="tracklist-title" class="font-bold">Track list</h2>
								<p class="text-sm text-base-content/55">Positions stay tied to the local album</p>
							</div>
						</div>
						<div class="divide-y divide-base-content/10">
							{#each currentDraft.media as medium (medium.position)}
								<div class="p-5 sm:p-6">
									<div class="mb-3 flex items-center gap-2">
										<Disc3 class="h-4 w-4 text-base-content/45" />
										<h3 class="text-sm font-bold uppercase tracking-wide">
											Medium {medium.position}
										</h3>
									</div>
									<div class="mb-4 grid gap-3 sm:grid-cols-2">
										<label class="form-control">
											<span class="label-text mb-1 text-xs font-semibold">Medium title</span>
											<input
												class="input input-sm input-bordered w-full"
												value={medium.title.value ?? ''}
												disabled={!canEdit}
												oninput={(event) => changeField(medium.title, event.currentTarget.value)}
											/>
										</label>
										<label class="form-control">
											<span class="label-text mb-1 text-xs font-semibold">MusicBrainz format</span>
											<input
												class="input input-sm input-bordered w-full"
												value={medium.format.value ?? ''}
												disabled={!canEdit}
												placeholder="For example, CD or 12&quot; Vinyl"
												oninput={(event) => changeField(medium.format, event.currentTarget.value)}
											/>
										</label>
									</div>
									<div class="space-y-2">
										{#each medium.tracks as track (track.local_track_id)}
											<div
												class="grid gap-2 rounded-box bg-base-200/45 p-3 sm:grid-cols-[3rem_minmax(0,1fr)_minmax(10rem,0.65fr)] sm:items-center"
											>
												<span class="text-sm font-bold tabular-nums text-base-content/45">
													{track.disc_number}.{track.track_number || '–'}
												</span>
												<label class="form-control">
													<span class="sr-only">Title for track {track.track_number}</span>
													<input
														class="input input-sm input-bordered w-full"
														value={track.title.value ?? ''}
														disabled={!canEdit}
														oninput={(event) => changeField(track.title, event.currentTarget.value)}
													/>
												</label>
												<label class="form-control">
													<span class="sr-only">Artist for track {track.track_number}</span>
													<input
														class="input input-sm input-bordered w-full"
														value={track.artist_name.value ?? ''}
														disabled={!canEdit}
														oninput={(event) =>
															changeField(track.artist_name, event.currentTarget.value)}
													/>
												</label>
											</div>
										{/each}
									</div>
								</div>
							{/each}
						</div>
					</section>

					{#if contribution.validation.length}
						<section
							class="rounded-box border border-warning/30 bg-warning/10 p-5"
							aria-labelledby="validation-title"
						>
							<h2 id="validation-title" class="flex items-center gap-2 font-bold">
								<AlertTriangle class="h-4 w-4 text-warning" /> Still needed
							</h2>
							<ul class="mt-2 list-inside list-disc text-sm text-base-content/65">
								{#each contribution.validation as issue (issue.code + issue.field)}
									<li>{issue.message}</li>
								{/each}
							</ul>
						</section>
					{/if}

					<ContributionMusicBrainzReview {contribution} canMutate={canUseProviderActions} />

					{#if authStore.isTrusted && (contribution.next_actions.includes('cancel') || canEdit)}
						<div
							class="sticky bottom-3 z-10 flex flex-wrap items-center justify-between gap-3 rounded-box border border-base-content/15 bg-base-100/95 p-3 shadow-xl backdrop-blur sm:p-4"
						>
							<div class="flex items-center gap-2 text-sm text-base-content/55">
								<FilePenLine class="h-4 w-4" /> Changes stay in this contribution draft
							</div>
							<div class="flex items-center gap-2">
								{#if contribution.next_actions.includes('cancel')}
									<button
										class="btn btn-ghost btn-sm gap-2"
										onclick={() => cancelDialog?.showModal()}
									>
										<Trash2 class="h-4 w-4" /> Cancel
									</button>
								{/if}
								<button
									class="btn btn-primary btn-sm gap-2"
									disabled={!canEdit || saveMutation.isPending}
									onclick={save}
								>
									{#if saveMutation.isPending}<LoaderCircle
											class="h-4 w-4 animate-spin"
										/>{:else}<Save class="h-4 w-4" />{/if}
									Save draft
								</button>
							</div>
						</div>
					{/if}
				{/if}
			</div>
		</div>
	{/if}
</main>

<dialog bind:this={cancelDialog} class="modal">
	<div class="modal-box">
		<h2 class="text-lg font-bold">Cancel this contribution?</h2>
		<p class="mt-2 text-sm text-base-content/65">
			The draft will close. Your library and audio files won't be changed.
		</p>
		<div class="modal-action">
			<button class="btn btn-ghost" onclick={() => cancelDialog?.close()}>Keep draft</button>
			<button class="btn btn-error gap-2" disabled={cancelMutation.isPending} onclick={cancel}>
				{#if cancelMutation.isPending}<LoaderCircle class="h-4 w-4 animate-spin" />{/if}
				Cancel contribution
			</button>
		</div>
	</div>
	<form method="dialog" class="modal-backdrop"><button aria-label="Close">close</button></form>
</dialog>
