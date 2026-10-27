<!--
	One edition, compared with the person's files: what it is, how the files
	fit it, and (for the current edition) the files it has no place for.
	The primary action comes from the sheet, which also shows it in the
	phone's bottom bar.
-->
<script lang="ts">
	import type { Snippet } from 'svelte';
	import { ExternalLink, EyeOff, Trash2, Undo2 } from 'lucide-svelte';
	import { ApiError } from '$lib/api/client';
	import { coverPath } from '$lib/api/covers';
	import AlbumImage from '$lib/components/AlbumImage.svelte';
	import { outsideLink } from '$lib/components/outsideLink';
	import AsyncButton from '$lib/ui/AsyncButton.svelte';
	import Button from '$lib/ui/Button.svelte';
	import Card from '$lib/ui/Card.svelte';
	import QueryBoundary from '$lib/ui/QueryBoundary.svelte';
	import ToggleRow from '$lib/ui/ToggleRow.svelte';
	import { withBasePath } from '$lib/utils/basePath';
	import type { EditionPlan } from '$lib/queries/albums/EditionQueries.svelte';
	import EditionCompare from './EditionCompare.svelte';
	import EditionDetailSkeleton from './EditionDetailSkeleton.svelte';
	import EditionTracks, { type TracksOutcome } from './EditionTracks.svelte';
	import {
		GET_MISSING_HELP,
		MUSICBRAINZ_DOWN,
		NO_SOURCE_SENTENCE,
		REPLACE_NEEDS_MISSING_HELP,
		REPLACE_VERSIONS_HELP,
		editionName,
		editionYear,
		extrasButton,
		formatsLabel,
		getMissingLabel,
		otherAlbumSentence,
		putBackButton,
		replaceVersionsLabel,
		summarySentence,
		trulyMissing
	} from './editionCopy';

	interface Props {
		release: string;
		query: {
			data: EditionPlan | undefined;
			isPending: boolean;
			isError: boolean;
			error: unknown;
			refetch: () => unknown;
		};
		/** The album's current edition. */
		isCurrent: boolean;
		hiddenByFilter: boolean;
		/** The album sits in a release group, so an edition of another one moves it. */
		albumHasGroup: boolean;
		canEdit: boolean;
		primary?: Snippet;
		actionError: string | null;
		putBackCount: number | null;
		onremoveextras: (trackIds: string[]) => Promise<void>;
		onputback: () => Promise<void>;
		/** A download source is set up. */
		sourceReady: boolean;
		/** Choosing another edition also gets its missing tracks. */
		getMissing: boolean;
		/** ...and replaces the tracks the files hold in another version. */
		replaceVersions: boolean;
		/** The release group the current edition's tracks are fetched under. */
		group: string;
		tracks: TracksOutcome | null;
		/** Asks for the current edition's tracks. */
		onacquire: (replaceVersions: boolean) => Promise<void>;
		/** The album's edition kind from its status: `release`, `custom`, ... */
		editionKind: string | null;
		/** The app picked the album's edition (a best guess or confirmed). */
		appPicked: boolean;
		onmatch: (trackId: string, releaseTrackMbid: string, identityRevision: number) => Promise<void>;
		onunmatch: (trackId: string) => Promise<void>;
	}

	let {
		release,
		query,
		isCurrent,
		hiddenByFilter,
		albumHasGroup,
		canEdit,
		primary,
		actionError,
		putBackCount,
		onremoveextras,
		onputback,
		sourceReady,
		getMissing = $bindable(),
		replaceVersions = $bindable(),
		group,
		tracks,
		onacquire,
		editionKind,
		appPicked,
		onmatch,
		onunmatch
	}: Props = $props();

	const canMatch = $derived(isCurrent && canEdit && editionKind === 'release');

	let confirmExtras = $state(false);

	const extras = $derived(query.data?.files.filter((file) => file.fate !== 'keep') ?? []);
	function musicBrainzDown(error: unknown): boolean {
		return error instanceof ApiError && (error.status === 0 || error.status >= 500);
	}
	const musicBrainzLink = $derived(outsideLink(`https://musicbrainz.org/release/${release}`));

	$effect(() => {
		void release;
		confirmExtras = false;
	});

	async function removeExtras(): Promise<void> {
		await onremoveextras(extras.map((file) => file.local_track_id));
		confirmExtras = false;
	}
</script>

<div class="flex flex-col gap-5">
	{#if hiddenByFilter}
		<p class="flex items-center gap-2 text-xs text-base-content/60">
			<EyeOff class="h-3.5 w-3.5" aria-hidden="true" /> Hidden by your filter
		</p>
	{/if}

	<QueryBoundary {query} what="this edition">
		{#snippet loading()}<EditionDetailSkeleton />{/snippet}
		{#snippet failed(error, retry)}
			<div class="flex flex-col items-start gap-2 text-sm" role="alert">
				<p>
					{musicBrainzDown(error)
						? MUSICBRAINZ_DOWN
						: error instanceof Error
							? error.message
							: "This edition couldn't be compared with your files."}
				</p>
				<Button variant="outline" size="sm" onclick={retry}>Retry</Button>
			</div>
			{#if primary}
				<div class="hidden flex-wrap items-center gap-3 md:flex">
					{@render primary()}
				</div>
			{/if}
		{/snippet}
		{#snippet children(plan)}
			{@const missing = trulyMissing(plan)}
			{@const versions = plan.other_version_count}
			<header class="flex items-start gap-4">
				<AlbumImage
					customUrl={coverPath('release', plan.release_mbid, 250)}
					alt=""
					size="full"
					className="h-20 w-20 shrink-0 rounded-box"
					retryOnError={false}
				/>
				<div class="min-w-0">
					<h3 class="text-lg font-bold">{editionName(plan.edition)}</h3>
					<p class="text-sm text-base-content/70">
						{[
							plan.artist_name,
							editionYear(plan.edition.date) ? `Released ${editionYear(plan.edition.date)}` : null,
							plan.edition.country,
							formatsLabel(plan.edition.formats)
						]
							.filter(Boolean)
							.join(' · ')}
					</p>
					<p class="text-xs text-base-content/55">
						{#each [plan.edition.label, plan.edition.catalog_number, plan.edition.barcode].filter(Boolean) as fact (fact)}
							<span>{fact} · </span>
						{/each}
						{#if musicBrainzLink}
							<a
								class="link inline-flex items-center gap-1"
								href={musicBrainzLink}
								target="_blank"
								rel="noreferrer"
								>Open in MusicBrainz <ExternalLink class="h-3 w-3" aria-hidden="true" /></a
							>
						{/if}
					</p>
				</div>
			</header>

			<p class="text-sm">{summarySentence(plan)}</p>

			<EditionCompare {plan} {canMatch} {appPicked} {onmatch} {onunmatch} />

			{#if !plan.same_album && albumHasGroup && !isCurrent}
				<p class="text-sm text-base-content/75">
					{otherAlbumSentence(plan.title, plan.artist_name)}
				</p>
			{/if}

			{#if canEdit && (missing > 0 || versions > 0) && (!isCurrent || group)}
				<Card
					as="section"
					variant="bordered"
					padding="md"
					aria-labelledby="edition-make-copy-title"
				>
					<h4 id="edition-make-copy-title" class="text-sm font-semibold">
						Make your copy this edition
					</h4>
					{#if isCurrent}
						<EditionTracks
							{group}
							localId={plan.album_id}
							{plan}
							{missing}
							{versions}
							{sourceReady}
							outcome={tracks}
							{onacquire}
						/>
					{:else}
						{#if missing > 0}
							<ToggleRow
								label={getMissingLabel(missing)}
								description={GET_MISSING_HELP}
								bind:checked={getMissing}
							/>
						{/if}
						{#if versions > 0}
							<ToggleRow
								label={replaceVersionsLabel(versions)}
								description={missing > 0 && !getMissing
									? REPLACE_NEEDS_MISSING_HELP
									: REPLACE_VERSIONS_HELP}
								disabled={missing > 0 && !getMissing}
								bind:checked={replaceVersions}
							/>
						{/if}
						{#if !sourceReady}
							<p class="text-sm text-base-content/70">
								{NO_SOURCE_SENTENCE}
								<a class="link" href={withBasePath('/settings?tab=download-client')}
									>Download settings</a
								>
							</p>
						{/if}
					{/if}
				</Card>
			{/if}
			{#if primary}
				<div class="hidden flex-wrap items-center gap-3 md:flex">
					{@render primary()}
				</div>
			{/if}
			{#if actionError}
				<p class="text-sm text-error" role="alert">{actionError}</p>
			{/if}

			{#if isCurrent && canEdit && (extras.length > 0 || putBackCount)}
				<section class="flex flex-col gap-2 text-sm" aria-label="Files not on this edition">
					{#if extras.length > 0 && !confirmExtras}
						<Button
							variant="ghost"
							size="sm"
							icon={Trash2}
							class="self-start"
							onclick={() => (confirmExtras = true)}>{extrasButton(extras.length)}</Button
						>
					{/if}
					{#if confirmExtras}
						<div class="rounded-box border border-warning/40 bg-warning/5 p-3">
							<ul class="list-disc pl-5 text-base-content/75">
								{#each extras as file (file.local_track_id)}
									<li>
										{file.title || 'Untitled'}
										<span class="text-base-content/50"
											>({file.fate === 'duplicate'
												? 'second copy'
												: file.fate === 'other_version'
													? 'another version'
													: 'not on this edition'})</span
										>
									</li>
								{/each}
							</ul>
							<p class="mt-1 text-xs text-base-content/60">
								Nothing is deleted: the files go to the recycle bin, and you can put them back.
							</p>
							<div class="mt-2 flex flex-wrap gap-2">
								<AsyncButton variant="warning" size="sm" onclick={removeExtras}
									>Move to recycle bin</AsyncButton
								>
								<Button variant="ghost" size="sm" onclick={() => (confirmExtras = false)}
									>Keep them</Button
								>
							</div>
						</div>
					{/if}
					{#if putBackCount}
						<AsyncButton
							variant="ghost"
							size="sm"
							icon={Undo2}
							class="self-start"
							pendingLabel="Putting back..."
							onclick={onputback}>{putBackButton(putBackCount)}</AsyncButton
						>
					{/if}
				</section>
			{/if}
		{/snippet}
	</QueryBoundary>
</div>
