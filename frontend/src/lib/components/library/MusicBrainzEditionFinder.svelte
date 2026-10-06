<script lang="ts">
	import {
		ArrowLeft,
		ArrowRight,
		BadgeCheck,
		Disc3,
		ExternalLink,
		Search,
		ShieldCheck
	} from 'lucide-svelte';

	import AlbumImage from '$lib/components/AlbumImage.svelte';
	import { coverPath } from '$lib/api/covers';
	import { getReleaseEditionSearchQuery } from '$lib/queries/library/LibraryEditionQueries.svelte';
	import { authStore } from '$lib/stores/authStore.svelte';

	interface Props {
		albumId: string;
		artistName: string;
		albumTitle: string;
		currentReleaseMbid?: string | null;
		mappedTrackCount?: number;
		totalTrackCount?: number;
		oncheck: (releaseMbid: string) => void | Promise<void>;
		checking?: boolean;
	}

	let {
		albumId,
		artistName,
		albumTitle,
		currentReleaseMbid = null,
		mappedTrackCount = 0,
		totalTrackCount = 0,
		oncheck,
		checking = false
	}: Props = $props();
	let releaseTitleInput = $state('');
	let artistInput = $state('');
	let submittedTitle = $state('');
	let submittedArtist = $state('');
	let offset = $state(0);
	let suggestionsOpen = $state(false);
	let initialized = $state(false);
	let releaseTitleField: HTMLInputElement | undefined;
	let advancedValue = $state('');
	let advancedAttempted = $state(false);
	// Set while the list shows every release of the album's release group.
	let releaseGroup = $state<string | null>(null);
	let knownReleaseGroup = $state<string | null>(null);

	const searchQuery = getReleaseEditionSearchQuery(
		() => authStore.user?.id,
		() => albumId,
		() => submittedTitle,
		() => submittedArtist,
		() => offset,
		() => suggestionsOpen,
		() => releaseGroup
	);
	const result = $derived(searchQuery.data);
	$effect(() => {
		if (result?.current_release_group_mbid) knownReleaseGroup = result.current_release_group_mbid;
	});
	const currentEdition = $derived(
		result?.items.find((edition) => edition.is_current_release) ?? null
	);
	const suggestedEditions = $derived(
		result?.items.filter((edition) => !edition.is_current_release) ?? []
	);
	const hasPrevious = $derived(offset > 0);
	const hasNext = $derived(Boolean(result && result.offset + result.items.length < result.total));
	const musicBrainzQuery = $derived(buildMusicBrainzQuery(submittedTitle, submittedArtist));
	const musicBrainzSearchUrl = $derived(
		releaseGroup
			? `https://musicbrainz.org/release-group/${releaseGroup}`
			: `https://musicbrainz.org/search?query=${encodeURIComponent(musicBrainzQuery)}&type=release&method=indexed`
	);
	const parsedAdvancedMbid = $derived(parseMusicBrainzReleaseId(advancedValue));

	$effect(() => {
		if (initialized) return;
		releaseTitleInput = albumTitle;
		artistInput = artistName;
		submittedTitle = albumTitle.trim();
		submittedArtist = artistName.trim();
		suggestionsOpen = !currentReleaseMbid;
		initialized = true;
	});

	export function parseMusicBrainzReleaseId(value: string): string | null {
		const normalized = value.trim().toLowerCase();
		const match = normalized.match(
			/^(?:https?:\/\/(?:www\.)?musicbrainz\.org\/release\/)?([0-9a-f]{8}-[0-9a-f]{4}-[1-5][0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12})(?:[/?#].*)?$/
		);
		return match?.[1] ?? null;
	}

	function escapeLucenePhrase(value: string): string {
		return value.replace(/[+\-&|!(){}[\]^"~*?:\\/]/g, '\\$&');
	}

	function buildMusicBrainzQuery(title: string, artist: string): string {
		const release = `release:"${escapeLucenePhrase(title)}"`;
		return artist ? `${release} AND artist:"${escapeLucenePhrase(artist)}"` : release;
	}

	function search(): void {
		const normalizedTitle = releaseTitleInput.trim();
		if (!normalizedTitle) return;
		releaseGroup = null;
		offset = 0;
		submittedTitle = normalizedTitle;
		submittedArtist = artistInput.trim();
	}

	function listReleaseGroup(group: string | null): void {
		releaseGroup = group;
		offset = 0;
	}

	function toggleReplacement(event: Event & { currentTarget: HTMLDetailsElement }): void {
		suggestionsOpen = event.currentTarget.open;
		if (suggestionsOpen) requestAnimationFrame(() => releaseTitleField?.focus());
	}

	function checkAdvanced(): void {
		advancedAttempted = true;
		if (parsedAdvancedMbid) void oncheck(parsedAdvancedMbid);
	}
</script>

{#snippet editionSuggestions()}
	<form
		class="edition-finder-search"
		onsubmit={(event) => {
			event.preventDefault();
			search();
		}}
	>
		<div class="edition-finder-fields">
			<label>
				<span>Artist <small>optional</small></span>
				<input class="input input-bordered w-full" bind:value={artistInput} autocomplete="off" />
			</label>
			<label>
				<span>Release title</span>
				<input
					class="input input-bordered w-full"
					bind:this={releaseTitleField}
					bind:value={releaseTitleInput}
					autocomplete="off"
					required
				/>
			</label>
		</div>
		<button class="btn btn-primary gap-2" type="submit" disabled={!releaseTitleInput.trim()}>
			<Search class="h-4 w-4" /> Search
		</button>
	</form>
	{#if releaseGroup}
		<p class="text-sm text-base-content/60">
			Showing every edition of this album on MusicBrainz.
			<button class="btn btn-link btn-sm px-1" onclick={() => listReleaseGroup(null)}
				>Back to search results</button
			>
		</p>
	{:else if knownReleaseGroup}
		<button
			class="btn btn-ghost btn-sm self-start"
			onclick={() => listReleaseGroup(knownReleaseGroup)}>Show every edition of this album</button
		>
	{/if}

	{#if searchQuery.isLoading || searchQuery.isFetching}
		<div class="edition-finder-list" aria-live="polite" aria-label="Loading MusicBrainz editions">
			{#each Array(3) as _, index (index)}<div class="skeleton h-24 rounded-xl"></div>{/each}
		</div>
	{:else if searchQuery.isError}
		<div class="edition-finder-state" role="alert">
			<Disc3 class="h-7 w-7 text-error" />
			<div>
				<strong>MusicBrainz is unavailable</strong>
				<p>No album evidence was changed. Retry here or open this search on MusicBrainz.</p>
			</div>
			<button
				class="btn btn-outline btn-sm ml-auto"
				disabled={searchQuery.isFetching}
				onclick={() => void searchQuery.refetch()}>Retry</button
			>
		</div>
	{:else if result && suggestedEditions.length === 0}
		<div class="edition-finder-state" aria-live="polite">
			<Disc3 class="h-7 w-7" />
			<div>
				<strong>{currentEdition ? 'No different editions found' : 'No editions found'}</strong>
				<p>Adjust the artist or release title.</p>
			</div>
		</div>
	{:else if result}
		<div class="edition-finder-list" aria-live="polite">
			{#each suggestedEditions as edition (edition.release_mbid)}
				<article class="edition-finder-row">
					<AlbumImage
						customUrl={coverPath('release', edition.release_mbid, 250)}
						alt={`Cover for ${edition.title}`}
						size="sm"
						className="edition-finder-art border border-base-content/10"
						retryOnError={false}
					/>
					<div class="edition-finder-copy">
						<div class="edition-finder-title">
							<strong>{edition.title}</strong>{#if edition.belongs_to_current_release_group}<span
									class="badge badge-primary badge-sm">Current release group</span
								>{/if}
						</div>
						<p class="text-sm text-base-content/60">
							{edition.artist_name || 'Unknown artist'} · {[
								edition.date,
								edition.country,
								edition.status
							]
								.filter(Boolean)
								.join(' · ') || 'Undated edition'}
						</p>
						<p class="mt-1 text-xs text-base-content/50">
							{[
								edition.media_formats.join(' + '),
								edition.packaging,
								edition.disc_count
									? `${edition.disc_count} disc${edition.disc_count === 1 ? '' : 's'}`
									: null,
								edition.track_count ? `${edition.track_count} tracks` : null,
								edition.label,
								edition.catalogue_number,
								edition.barcode
							]
								.filter(Boolean)
								.join(' · ')}
						</p>
						{#if edition.disambiguation}<p class="mt-1 text-xs italic text-base-content/55">
								{edition.disambiguation}
							</p>{/if}
					</div>
					<div class="edition-finder-action">
						<span class="text-xs text-base-content/45">{edition.score}% match</span><button
							class="btn btn-outline btn-sm"
							disabled={checking}
							onclick={() => void oncheck(edition.release_mbid)}
							>Check this edition <ArrowRight class="h-4 w-4" /></button
						>
					</div>
				</article>
			{/each}
		</div>
	{/if}

	<div class="edition-finder-footer">
		<div class="join">
			<button
				class="btn btn-sm join-item"
				disabled={!hasPrevious || searchQuery.isFetching}
				onclick={() => (offset = Math.max(0, offset - 12))}
				><ArrowLeft class="h-4 w-4" /> Previous</button
			>
			<button
				class="btn btn-sm join-item"
				disabled={!hasNext || searchQuery.isFetching}
				onclick={() => (offset += 12)}>Next <ArrowRight class="h-4 w-4" /></button
			>
		</div>
		<a class="btn btn-ghost btn-sm" href={musicBrainzSearchUrl} target="_blank" rel="noreferrer"
			>Open this search on MusicBrainz <ExternalLink class="h-4 w-4" /></a
		>
	</div>

	<details class="edition-finder-advanced">
		<summary>Already know the release? Paste a UUID or canonical MusicBrainz URL</summary>
		<div class="mt-3 flex flex-col gap-2 sm:flex-row">
			<label class="input input-bordered flex flex-1 items-center gap-2"
				><ShieldCheck class="h-4 w-4" /><span class="sr-only">MusicBrainz release UUID or URL</span
				><input
					bind:value={advancedValue}
					class="min-w-0 flex-1 font-mono text-sm"
					placeholder="https://musicbrainz.org/release/…"
				/></label
			>
			<button class="btn btn-outline" disabled={checking} onclick={checkAdvanced}
				>Check exact release</button
			>
		</div>
		{#if advancedAttempted && !parsedAdvancedMbid}<p class="mt-2 text-sm text-error" role="alert">
				Enter a MusicBrainz release UUID or canonical release URL.
			</p>{/if}
	</details>
{/snippet}

<section class="edition-finder" aria-labelledby="edition-finder-title">
	<div class="edition-finder-heading">
		<div>
			<p class="identification-kicker">MusicBrainz edition finder</p>
			<h3 id="edition-finder-title" class="hero-title text-xl font-bold">
				{currentReleaseMbid ? 'Attached exact release' : 'Search exact releases'}
			</h3>
		</div>
		<span class="edition-finder-suggestion">Catalog only</span>
	</div>

	{#if currentReleaseMbid}
		<article class="edition-finder-current">
			<AlbumImage
				customUrl={coverPath('release', currentReleaseMbid, 250)}
				alt={`Cover for ${currentEdition?.title ?? albumTitle}`}
				size="sm"
				className="edition-finder-current-art border border-base-content/10"
				retryOnError={false}
			/>
			<div class="min-w-0 flex-1">
				<div class="flex flex-wrap items-center gap-2">
					<span class="badge badge-success badge-sm gap-1"
						><BadgeCheck class="h-3 w-3" /> Currently attached</span
					>
					<strong class="hero-title truncate">{currentEdition?.title ?? albumTitle}</strong>
				</div>
				<p class="mt-1 text-sm text-base-content/60">
					{currentEdition?.artist_name || artistName || 'Unknown artist'}
				</p>
				<p class="mt-1 text-xs text-base-content/50">
					{mappedTrackCount} of {totalTrackCount} indexed
					{totalTrackCount === 1 ? 'file' : 'files'} mapped
				</p>
			</div>
			<a
				class="btn btn-ghost btn-sm"
				href={`https://musicbrainz.org/release/${currentReleaseMbid}`}
				target="_blank"
				rel="noreferrer">MusicBrainz <ExternalLink class="h-4 w-4" /></a
			>
		</article>

		<details class="edition-finder-replacement" ontoggle={toggleReplacement}>
			<summary>Choose a different edition</summary>
			<div class="edition-finder-replacement-body">{@render editionSuggestions()}</div>
		</details>
	{:else}
		{@render editionSuggestions()}
	{/if}
</section>
