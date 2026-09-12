<script lang="ts">
	import { TriangleAlert, Check, ExternalLink, GitCompareArrows } from 'lucide-svelte';
	import type { LibraryContribution, ReleaseDraft } from '$lib/types';

	type ComparisonField =
		| 'title'
		| 'artist_credit'
		| 'release_date'
		| 'country'
		| 'label'
		| 'catalogue_number'
		| 'barcode';

	interface Props {
		contribution: LibraryContribution;
		draft: ReleaseDraft;
		canMutate: boolean;
		onuse: (field: ComparisonField, value: string | null, source: 'local' | 'discogs') => void;
		onusemedium: (
			position: number,
			field: 'title' | 'format',
			value: string | null,
			source: 'local' | 'discogs'
		) => void;
		onusetrack: (
			localTrackId: string,
			title: string | null,
			artistName: string | null,
			source: 'local' | 'discogs'
		) => void;
	}

	let { contribution, draft, canMutate, onuse, onusemedium, onusetrack }: Props = $props();
	const release = $derived(contribution.discogs_source?.release ?? null);
	const firstLabel = $derived(release?.labels[0] ?? null);
	const rows = $derived(
		release
			? [
					{
						key: 'title' as const,
						label: 'Release title',
						local: contribution.local_snapshot.title,
						discogs: release.title
					},
					{
						key: 'artist_credit' as const,
						label: 'Artist credit',
						local: contribution.local_snapshot.album_artist_name,
						discogs: release.artist_name
					},
					{
						key: 'release_date' as const,
						label: 'Release date',
						local:
							contribution.local_snapshot.release_date ??
							contribution.local_snapshot.year?.toString() ??
							null,
						discogs: release.released_date
					},
					{ key: 'country' as const, label: 'Country', local: null, discogs: release.country },
					{ key: 'label' as const, label: 'Label', local: null, discogs: firstLabel?.name ?? null },
					{
						key: 'catalogue_number' as const,
						label: 'Catalogue number',
						local: null,
						discogs: firstLabel?.catalogue_number ?? null
					},
					{ key: 'barcode' as const, label: 'Barcode', local: null, discogs: release.barcode }
				]
			: []
	);

	function durationLabel(seconds: number | null | undefined): string {
		if (!seconds || seconds <= 0) return 'Duration not listed';
		const minutes = Math.floor(seconds / 60);
		return `${minutes}:${Math.round(seconds % 60)
			.toString()
			.padStart(2, '0')}`;
	}
</script>

{#if release}
	<section
		class="rounded-box border border-base-content/10 bg-base-100"
		aria-labelledby="compare-title"
	>
		<div
			class="flex flex-wrap items-start justify-between gap-3 border-b border-base-content/10 px-5 py-4 sm:px-6"
		>
			<div class="flex items-start gap-3">
				<GitCompareArrows class="mt-0.5 h-5 w-5 text-primary" />
				<div>
					<h2 id="compare-title" class="font-bold">Compare edition metadata</h2>
					<p class="text-sm text-base-content/55">
						Choose only the Discogs facts that belong to this exact edition.
					</p>
				</div>
			</div>
			<a
				class="link link-primary inline-flex items-center gap-1 text-xs"
				href={release.canonical_release_url}
				target="_blank"
				rel="noopener noreferrer"
			>
				Data provided by Discogs <ExternalLink class="h-3 w-3" />
			</a>
		</div>

		<div
			class="hidden grid-cols-[11rem_minmax(0,1fr)_minmax(0,1fr)_10rem] gap-px bg-base-content/10 text-sm md:grid"
		>
			<div class="bg-base-200 px-4 py-3 font-bold">Field</div>
			<div class="bg-base-200 px-4 py-3 font-bold">Local metadata</div>
			<div class="bg-base-200 px-4 py-3 font-bold">Discogs</div>
			<div class="bg-base-200 px-4 py-3 font-bold">Use for MusicBrainz</div>
			{#each rows as row (row.key)}
				<div class="bg-base-100 px-4 py-3 font-semibold">{row.label}</div>
				<div class="bg-base-100 px-4 py-3 text-base-content/65">
					{row.local ?? 'Not in local metadata'}
				</div>
				<div class="bg-base-100 px-4 py-3 text-base-content/65">{row.discogs ?? 'Not listed'}</div>
				<div class="grid gap-1 bg-base-100 px-4 py-2">
					<button
						class="btn btn-xs {draft[row.key].source === 'local' ? 'btn-primary' : 'btn-outline'}"
						disabled={!canMutate}
						aria-pressed={draft[row.key].source === 'local'}
						onclick={() => onuse(row.key, row.local, 'local')}
					>
						Local
					</button>
					<button
						class="btn btn-xs {draft[row.key].source === 'discogs' ? 'btn-primary' : 'btn-outline'}"
						disabled={!canMutate || row.discogs === null}
						aria-pressed={draft[row.key].source === 'discogs'}
						onclick={() => onuse(row.key, row.discogs, 'discogs')}
					>
						Discogs
					</button>
				</div>
			{/each}
		</div>

		<div class="divide-y divide-base-content/10 md:hidden">
			{#each rows as row (row.key)}
				<fieldset class="p-5">
					<legend class="font-bold">{row.label}</legend>
					<dl class="mt-3 space-y-2 text-sm">
						<div>
							<dt class="text-xs text-base-content/45">Local metadata</dt>
							<dd>{row.local ?? 'Not listed'}</dd>
						</div>
						<div>
							<dt class="text-xs text-base-content/45">Discogs</dt>
							<dd>{row.discogs ?? 'Not listed'}</dd>
						</div>
					</dl>
					<div class="mt-3 flex gap-2">
						<button
							class="btn btn-sm {draft[row.key].source === 'local' ? 'btn-primary' : 'btn-outline'}"
							disabled={!canMutate}
							aria-pressed={draft[row.key].source === 'local'}
							onclick={() => onuse(row.key, row.local, 'local')}>Use local</button
						>
						<button
							class="btn btn-sm {draft[row.key].source === 'discogs'
								? 'btn-primary'
								: 'btn-outline'}"
							disabled={!canMutate || row.discogs === null}
							aria-pressed={draft[row.key].source === 'discogs'}
							onclick={() => onuse(row.key, row.discogs, 'discogs')}>Use Discogs</button
						>
					</div>
				</fieldset>
			{/each}
		</div>

		<div class="border-t border-base-content/10 p-5 sm:p-6">
			<h3 class="font-bold">Media details</h3>
			<div class="mt-3 grid gap-3 md:grid-cols-2">
				{#each draft.media as medium (medium.position)}
					{@const providerMedium = release.media.find((item) => item.position === medium.position)}
					{@const localMedium = contribution.local_snapshot.media.find(
						(item) => item.position === medium.position
					)}
					<div class="rounded-box border border-base-content/10 bg-base-200/35 p-3 text-sm">
						<p class="font-semibold">Medium {medium.position}</p>
						<p class="mt-1 text-xs text-base-content/55">
							Local: {localMedium?.title ?? 'No title'} · no format inferred
						</p>
						<p class="text-xs text-base-content/55">
							Discogs: {providerMedium?.title ?? 'No title'} · {providerMedium?.format ??
								'No format'}
						</p>
						<div class="mt-3 flex gap-2">
							<button
								class="btn btn-xs {medium.title.source === 'local' &&
								medium.format.source === 'local'
									? 'btn-primary'
									: 'btn-outline'}"
								disabled={!canMutate}
								onclick={() => {
									onusemedium(medium.position, 'title', localMedium?.title ?? null, 'local');
									onusemedium(medium.position, 'format', null, 'local');
								}}>Use local</button
							>
							<button
								class="btn btn-xs {medium.title.source === 'discogs' ||
								medium.format.source === 'discogs'
									? 'btn-primary'
									: 'btn-outline'}"
								disabled={!canMutate || !providerMedium}
								onclick={() => {
									onusemedium(medium.position, 'title', providerMedium?.title ?? null, 'discogs');
									onusemedium(medium.position, 'format', providerMedium?.format ?? null, 'discogs');
								}}>Use Discogs</button
							>
						</div>
					</div>
				{/each}
			</div>
		</div>

		<div class="border-t border-base-content/10 p-5 sm:p-6">
			<h3 class="font-bold">Track alignment</h3>
			<p class="mt-1 text-sm text-base-content/55">
				Local positions are never reordered. Conflicts stay visible for review.
			</p>
			<ul class="mt-4 grid gap-2 lg:grid-cols-2">
				{#each contribution.source_selection.alignments as alignment (alignment.local_track_id)}
					{@const localTrack = contribution.local_snapshot.media
						.flatMap((medium) => medium.tracks)
						.find((track) => track.local_track_id === alignment.local_track_id)}
					{@const providerMedium = release.media.find(
						(medium) => medium.position === localTrack?.disc_number
					)}
					{@const providerTrack = providerMedium?.tracks.find(
						(track) => !track.heading && track.source_position === alignment.provider_position
					)}
					{@const trackDraft = draft.media
						.flatMap((medium) => medium.tracks)
						.find((track) => track.local_track_id === alignment.local_track_id)}
					{@const providerArtist =
						providerTrack?.artists[0]?.credited_name ??
						providerTrack?.artists[0]?.name ??
						release.artist_name}
					<li class="rounded-box bg-base-200/45 px-3 py-3 text-sm">
						<div class="flex items-start justify-between gap-3">
							<span class="min-w-0 font-semibold"
								>{localTrack?.disc_number}.{localTrack?.track_number} {localTrack?.title}</span
							>
							<span
								class="badge badge-sm gap-1 {alignment.classification === 'exact'
									? 'badge-success'
									: alignment.classification === 'partial'
										? 'badge-warning'
										: 'badge-error'}"
							>
								{#if alignment.classification === 'exact'}<Check
										class="h-3 w-3"
									/>{:else}<TriangleAlert class="h-3 w-3" />{/if}
								{alignment.classification}
								{alignment.provider_position ?? ''}
							</span>
						</div>
						<div class="mt-2 grid gap-2 text-xs text-base-content/55 sm:grid-cols-2">
							<p>
								Local: {localTrack?.artist_name ?? 'Artist not listed'} · {durationLabel(
									localTrack?.duration_seconds
								)}
							</p>
							<p>
								Discogs: {providerTrack?.title ?? 'Track not aligned'} · {durationLabel(
									providerTrack?.duration_seconds
								)}
							</p>
						</div>
						{#if trackDraft && localTrack}
							<div class="mt-3 flex gap-2">
								<button
									class="btn btn-xs {trackDraft.title.source === 'local'
										? 'btn-primary'
										: 'btn-outline'}"
									disabled={!canMutate}
									onclick={() =>
										onusetrack(
											alignment.local_track_id,
											localTrack.title,
											localTrack.artist_name,
											'local'
										)}>Use local</button
								>
								<button
									class="btn btn-xs {trackDraft.title.source === 'discogs'
										? 'btn-primary'
										: 'btn-outline'}"
									disabled={!canMutate || !providerTrack}
									onclick={() =>
										onusetrack(
											alignment.local_track_id,
											providerTrack?.title ?? null,
											providerArtist,
											'discogs'
										)}>Use Discogs</button
								>
							</div>
						{/if}
					</li>
				{/each}
			</ul>
		</div>
	</section>
{/if}
