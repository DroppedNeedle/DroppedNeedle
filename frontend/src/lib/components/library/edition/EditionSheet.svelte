<!--
	The one place to see and change a library album's edition: the editions
	on the left, the selected one compared with the files on the right, and
	after a choice what happens next. It closes only when the person closes
	it. On a phone the list and the detail are two steps of one sheet.
	With `request`, for an album the library does not hold, it picks the
	edition a request gets: the same list, and the selected edition's tracks.
-->
<script lang="ts">
	import { onDestroy, untrack } from 'svelte';
	import { ChevronLeft, ExternalLink } from 'lucide-svelte';
	import { coverPath } from '$lib/api/covers';
	import AlbumImage from '$lib/components/AlbumImage.svelte';
	import { outsideLink } from '$lib/components/outsideLink';
	import { formatDuration } from '$lib/utils/formatting';
	import Modal from '$lib/ui/Modal.svelte';
	import AsyncButton from '$lib/ui/AsyncButton.svelte';
	import Button from '$lib/ui/Button.svelte';
	import QueryBoundary from '$lib/ui/QueryBoundary.svelte';
	import SelectField from '$lib/ui/SelectField.svelte';
	import { authStore } from '$lib/stores/authStore.svelte';
	import { integrationStore } from '$lib/stores/integration';
	import type { LibraryAlbumSummary } from '$lib/types';
	import { getLibraryAlbumDetailQuery } from '$lib/queries/library/LibraryQueries.svelte';
	import { getReleaseEditionSearchQuery } from '$lib/queries/library/LibraryEditionQueries.svelte';
	import {
		acquireEdition,
		chooseAlbumEdition,
		confirmAlbumEdition,
		getAlbumEditionStatusQuery,
		getAlbumEditionsQuery,
		getEditionPlanQuery,
		getEditionTracksQuery,
		handBackAlbumEdition,
		leaveAlbumUnmanaged,
		matchEditionFile,
		removeEditionExtras,
		sealCustomEdition,
		undoAlbumEdition,
		unmatchEditionFile,
		type EditionChoice
	} from '$lib/queries/albums/EditionQueries.svelte';
	import {
		getAlbumRemovalsQuery,
		putBackRemoval
	} from '$lib/queries/library/RemovalQueries.svelte';
	import EditionList, { type EditionGroup, type EditionRow } from './EditionList.svelte';
	import EditionListSkeleton from './EditionListSkeleton.svelte';
	import EditionDetail from './EditionDetail.svelte';
	import EditionDetailSkeleton from './EditionDetailSkeleton.svelte';
	import EditionNextSteps from './EditionNextSteps.svelte';
	import EditionFind from './EditionFind.svelte';
	import EditionNoneRight from './EditionNoneRight.svelte';
	import type { TracksOutcome } from './EditionTracks.svelte';
	import { editionSheet, type EditionRequestPick } from './editionSheetState.svelte';
	import {
		LEFT_UNMANAGED_NOTE,
		SHEET_DESCRIPTION,
		count,
		editionFactsLine,
		editionName,
		failureSentence,
		fitLine,
		fitsNoFile,
		planFit,
		summarySentence,
		trulyMissing,
		type EditionFactsLike
	} from './editionCopy';

	interface Props {
		open: boolean;
		/** The album's library copies; the first is the one the page shows. */
		copies: LibraryAlbumSummary[];
		/** The release group the page shows, empty on a local album's page. */
		groupMbid: string;
		/** `movedTo` is the release group a choice moved the album to, if it did. */
		onclose?: (movedTo: string | null) => void;
		/** Pick the edition to request instead, for an album with no copies. */
		request?: EditionRequestPick;
	}

	let { open = $bindable(false), copies, groupMbid, onclose, request }: Props = $props();

	const userId = () => authStore.user?.id;
	const canEdit = $derived(authStore.isTrusted);
	const isAdmin = $derived(authStore.isAdmin);

	let copyId = $state('');
	let selected = $state<string | null>(null);
	let initialisedFor = $state('');
	let step = $state<'list' | 'detail'>('list');
	let view = $state<'compare' | 'next' | 'handedBack'>('compare');
	let chosen = $state<{
		name: string;
		release: string;
		choice: EditionChoice;
		gaps: { missing: number; versions: number };
	} | null>(null);
	let movedTo = $state<string | null>(null);
	let filter = $state('');
	let findTitle = $state('');
	let findArtist = $state('');
	let searchArgs = $state<{ title: string; artist: string } | null>(null);
	let actionError = $state<string | null>(null);
	let noneRight = $state<{ done: string | null; error: string | null }>({
		done: null,
		error: null
	});
	let fits = $state<Record<string, string>>({});
	let tracks = $state<TracksOutcome | null>(null);
	// "Make your copy this edition", for an edition that is not the current one.
	// Without a download source the download is held until one is saved.
	let getMissing = $state(true);
	let replaceVersions = $state(true);
	// "Any acquisition source is set up": slskd, Usenet or a plugin.
	const sourceReady = $derived($integrationStore.download_client);

	let wasOpen = false;
	$effect(() => {
		const isOpen = open;
		editionSheet.open = isOpen;
		if (isOpen && !wasOpen) untrack(reset);
		wasOpen = isOpen;
	});
	onDestroy(() => {
		if (open) editionSheet.open = false;
	});

	function reset(): void {
		copyId = copies[0]?.id ?? '';
		initialisedFor = '';
		selected = null;
		step = 'list';
		view = 'compare';
		chosen = null;
		movedTo = null;
		filter = '';
		findTitle = '';
		findArtist = '';
		searchArgs = null;
		actionError = null;
		noneRight = { done: null, error: null };
		tracks = null;
		if (request) selected = request.release;
	}

	const copy = $derived(copies.find((item) => item.id === copyId) ?? copies[0]);
	const statusQuery = getAlbumEditionStatusQuery(
		userId,
		() => copyId,
		() => open
	);
	const status = $derived(statusQuery.data?.album_id === copyId ? statusQuery.data : null);
	const albumQuery = getLibraryAlbumDetailQuery(() => (open ? copyId : ''));
	const hold = $derived(albumQuery.data?.management_hold ?? null);
	const group = $derived(status?.release_group_mbid ?? groupMbid);
	const currentRelease = $derived(
		status?.edition_kind === 'release' ? (status.release_mbid ?? null) : null
	);
	const currentPlanQuery = getEditionPlanQuery(
		userId,
		() => copyId,
		() => null,
		() => open && Boolean(currentRelease)
	);
	const currentPlan = $derived(
		currentPlanQuery.data?.release_mbid === currentRelease ? (currentPlanQuery.data ?? null) : null
	);
	const selectedIsCurrent = $derived(Boolean(selected) && selected === currentRelease);
	const selectedPlanQuery = getEditionPlanQuery(
		userId,
		() => copyId,
		() => (selectedIsCurrent ? null : selected),
		() => open && Boolean(selected)
	);
	const selectedPlan = $derived(
		selectedPlanQuery.data?.release_mbid === selected ? selectedPlanQuery.data : undefined
	);
	const editionsQuery = getAlbumEditionsQuery(
		userId,
		() => group,
		() => open && Boolean(group)
	);
	const searchQuery = getReleaseEditionSearchQuery(
		userId,
		() => copyId,
		() => searchArgs?.title ?? '',
		() => searchArgs?.artist ?? '',
		() => 0,
		() => open && canEdit && Boolean(searchArgs)
	);

	const choose = chooseAlbumEdition();
	const confirm = confirmAlbumEdition();
	const handBack = handBackAlbumEdition();
	const undo = undoAlbumEdition();
	const removeExtras = removeEditionExtras();
	const putBackFiles = putBackRemoval();
	// The newest removal of extras from this copy that can be put back.
	const removalsQuery = getAlbumRemovalsQuery(() => (open && canEdit ? copyId : ''));
	const lastRemoval = $derived(
		(removalsQuery.data ?? []).find((removal) => removal.kind === 'extras' && !removal.held) ?? null
	);
	const acquire = acquireEdition();
	const sealCustom = sealCustomEdition();
	const leaveUnmanaged = leaveAlbumUnmanaged();
	const matchFile = matchEditionFile();
	const unmatchFile = unmatchEditionFile();

	// A new copy starts over on its own edition.
	$effect(() => {
		void copyId;
		untrack(() => {
			selected = null;
			view = 'compare';
			chosen = null;
			actionError = null;
			noneRight = { done: null, error: null };
			tracks = null;
		});
	});

	// Open on the album's own edition, else the closest candidate. A local
	// album has no editions to list, so it searches by its own title and
	// artist straight away.
	$effect(() => {
		if (!open || !status || initialisedFor === copyId) return;
		initialisedFor = copyId;
		selected =
			currentRelease ??
			status.candidates.find((candidate) => candidate.release_mbid)?.release_mbid ??
			null;
		if (!group && copy) {
			findTitle = copy.title;
			findArtist = copy.artist_name;
			if (canEdit) searchArgs = { title: copy.title, artist: copy.artist_name };
		}
	});

	// Remember each plan's fit, so a row keeps it after the selection moves on.
	$effect(() => {
		for (const plan of [currentPlan, selectedPlan]) {
			const fit = plan ? planFit(plan) : null;
			if (plan && fit && untrack(() => fits[plan.release_mbid]) !== fit) {
				fits[plan.release_mbid] = fit;
			}
		}
	});

	const fileCount = $derived(currentPlan?.files.length ?? copy?.track_count ?? 0);
	const editions = $derived(editionsQuery.data?.items ?? []);
	const editionByRelease = $derived(new Map(editions.map((item) => [item.release_mbid, item])));

	function factsOf(release: string): EditionFactsLike | null {
		const item = editionByRelease.get(release);
		if (!item) return null;
		return {
			title: item.title,
			disambiguation: item.disambiguation,
			date: item.date,
			country: item.country,
			formats: item.media_formats,
			track_count: item.track_count
		};
	}

	// An album the library does not hold: every edition, the one the
	// request gets first.
	const requestGroups = $derived.by((): EditionGroup[] => {
		const pick = request;
		if (!pick) return [];
		const rows: EditionRow[] = [...editions]
			.sort((a, b) => (a.date ?? '').localeCompare(b.date ?? ''))
			.map((item) => {
				const facts = factsOf(item.release_mbid) ?? {};
				return {
					key: item.release_mbid,
					release: item.release_mbid,
					name: editionName(facts),
					facts: editionFactsLine(facts),
					fit: null,
					current: item.release_mbid === pick.release,
					bestGuess: false
				};
			});
		rows.sort((a, b) => Number(b.current) - Number(a.current));
		return [{ id: 'editions', label: 'Editions of this album', rows }];
	});

	const libraryGroups = $derived.by((): EditionGroup[] => {
		if (!status) return [];
		const current: EditionRow[] = [];
		if (status.edition_kind === 'custom') {
			current.push({
				key: 'custom',
				release: null,
				name: 'Your own track list',
				facts: `Kept as your files are · ${count(fileCount, 'track', 'tracks')}`,
				fit: null,
				current: true,
				bestGuess: false
			});
		} else if (currentRelease) {
			const facts =
				factsOf(currentRelease) ??
				(currentPlan
					? {
							...currentPlan.edition,
							track_count: currentPlan.edition.track_count
						}
					: { title: copy?.title });
			current.push({
				key: currentRelease,
				release: currentRelease,
				name: editionName(facts),
				facts: editionFactsLine(facts),
				fit: fits[currentRelease] ?? null,
				current: true,
				bestGuess: status.state === 'unconfirmed'
			});
		}
		const shown: (string | null)[] = current.map((row) => row.release);
		const closest: EditionRow[] = [];
		for (const candidate of status.candidates) {
			const release = candidate.release_mbid;
			if (!release || shown.includes(release)) continue;
			shown.push(release);
			const facts = factsOf(release);
			closest.push({
				key: release,
				release,
				name: facts ? editionName(facts) : candidate.album_title,
				facts: facts ? editionFactsLine(facts) : candidate.album_artist_name,
				fit: fits[release] ?? fitLine(candidate.matched_files, fileCount),
				current: false,
				bestGuess: false
			});
		}
		const other: EditionRow[] = [...editions]
			.sort((a, b) => (b.date ?? '').localeCompare(a.date ?? ''))
			.filter((item) => !shown.includes(item.release_mbid))
			.map((item) => {
				shown.push(item.release_mbid);
				const facts = factsOf(item.release_mbid) ?? {};
				return {
					key: item.release_mbid,
					release: item.release_mbid,
					name: editionName(facts),
					facts: editionFactsLine(facts),
					fit: fits[item.release_mbid] ?? null,
					current: false,
					bestGuess: false
				};
			});
		const found: EditionRow[] = (searchQuery.data?.items ?? [])
			.filter((item) => !shown.includes(item.release_mbid))
			.map((item) => ({
				key: `search-${item.release_mbid}`,
				release: item.release_mbid,
				name: item.disambiguation ? `${item.title} (${item.disambiguation})` : item.title,
				facts: [
					item.artist_name,
					editionFactsLine({
						date: item.date,
						country: item.country,
						formats: item.media_formats,
						track_count: item.track_count
					})
				]
					.filter(Boolean)
					.join(' · '),
				fit: fits[item.release_mbid] ?? null,
				current: false,
				bestGuess: false
			}));
		return [
			{ id: 'current', label: 'Yours now', rows: current },
			{ id: 'closest', label: 'Closest to your files', rows: closest },
			{ id: 'other', label: 'Other editions of this album', rows: other },
			{ id: 'search', label: 'Search results', rows: found }
		];
	});

	const groups = $derived(request ? requestGroups : libraryGroups);
	const allRows = $derived(groups.flatMap((item) => item.rows));
	const needle = $derived(filter.trim().toLowerCase());
	const visibleGroups = $derived(
		needle
			? groups.map((item) => ({
					...item,
					rows: item.rows.filter((row) => `${row.name} ${row.facts}`.toLowerCase().includes(needle))
				}))
			: groups
	);
	const hiddenByFilter = $derived(
		Boolean(needle && selected) &&
			allRows.some((row) => row.release === selected) &&
			!visibleGroups.some((item) => item.rows.some((row) => row.release === selected))
	);

	const reasonShown = $derived(
		status && (status.state === 'unmatched' || status.state === 'unconfirmed')
			? status.reason.message
			: null
	);
	const steppedBack = $derived(Boolean(hold && hold.kind !== 'your_choice'));
	const keepsOwnTags = $derived(status?.reason.code === 'FOLDER_KEEPS_TAGS');
	const letChooseLabel = $derived(
		!status || keepsOwnTags
			? null
			: status.state === 'chosen'
				? 'Let DroppedNeedle choose'
				: status.state === 'unidentified' || status.state === 'unmatched'
					? 'Let DroppedNeedle look again'
					: null
	);

	const primaryKind = $derived(
		!canEdit || !selected || view !== 'compare'
			? null
			: selectedIsCurrent
				? status?.state === 'unconfirmed'
					? 'keep'
					: null
				: 'use'
	);

	function select(release: string): void {
		selected = release;
		actionError = null;
		getMissing = true;
		replaceVersions = true;
		view = 'compare';
		step = 'detail';
	}

	async function useEdition(): Promise<void> {
		const release = selected;
		if (!release) return;
		actionError = null;
		const plan = selectedPlan;
		const name = plan ? editionName(plan.edition) : 'the edition you chose';
		const gaps = {
			missing: plan ? trulyMissing(plan) : 0,
			versions: plan?.other_version_count ?? 0
		};
		// The download always asks for the missing tracks, so the
		// replacements need it on when there are any.
		const fetchTracks =
			(getMissing && gaps.missing > 0) ||
			(replaceVersions && gaps.versions > 0 && (getMissing || gaps.missing === 0));
		const replace = replaceVersions;
		try {
			const choice = await choose.mutateAsync({
				userId: userId(),
				localId: copyId,
				rgMbid: group || undefined,
				releaseMbid: release
			});
			chosen = { name, release, choice, gaps };
			tracks = null;
			if (groupMbid && choice.release_group_mbid !== groupMbid) {
				movedTo = choice.release_group_mbid;
			}
			view = 'next';
			step = 'detail';
		} catch (error) {
			actionError = failureSentence(error, "This edition couldn't be chosen.");
			return;
		}
		if (fetchTracks) void acquireTracks(replace);
	}

	async function keepEdition(): Promise<void> {
		actionError = null;
		try {
			await confirm.mutateAsync({ userId: userId(), localId: copyId, rgMbid: group || undefined });
		} catch (error) {
			actionError = failureSentence(error, "This edition couldn't be kept.");
		}
	}

	async function letChoose(): Promise<void> {
		actionError = null;
		try {
			await handBack.mutateAsync({ userId: userId(), localId: copyId, rgMbid: group || undefined });
			view = 'handedBack';
			step = 'detail';
		} catch (error) {
			actionError = failureSentence(error, "DroppedNeedle couldn't look again.");
			step = 'detail';
		}
	}

	async function undoChange(): Promise<void> {
		actionError = null;
		try {
			await undo.mutateAsync({ userId: userId(), localId: copyId, rgMbid: group || undefined });
			chosen = null;
			movedTo = null;
			view = 'compare';
			initialisedFor = '';
			noneRight = { done: null, error: null };
		} catch (error) {
			actionError = failureSentence(error, "The change couldn't be undone.");
			step = 'detail';
		}
	}

	async function moveExtras(trackIds: string[]): Promise<void> {
		actionError = null;
		try {
			await removeExtras.mutateAsync({ localId: copyId, trackIds });
		} catch (error) {
			actionError = failureSentence(error, "The files couldn't be moved.");
		}
	}

	// A file matched to an edition track by hand, and its undo. A refusal
	// rejects, so the compare table shows it on the file's row.
	async function matchTrack(
		trackId: string,
		releaseTrackMbid: string,
		identityRevision: number
	): Promise<void> {
		await matchFile.mutateAsync({ localId: copyId, trackId, releaseTrackMbid, identityRevision });
	}

	async function unmatchTrack(trackId: string): Promise<void> {
		await unmatchFile.mutateAsync({ localId: copyId, trackId });
	}

	async function putBack(): Promise<void> {
		if (!lastRemoval) return;
		actionError = null;
		try {
			const answer = await putBackFiles.mutateAsync({
				albumId: copyId,
				removalId: lastRemoval.id,
				releaseGroupMbid: group || null
			});
			if (answer.missing.length > 0) {
				actionError = `Not in the recycle bin as it was, so not put back: ${answer.missing.join(', ')}`;
			}
		} catch (error) {
			actionError = failureSentence(error, "The files couldn't be put back.");
		}
	}

	// One download for the album's missing tracks (and, with
	// `replaceVersions`, this edition's version of the tracks the files
	// hold in another one), checked against its edition. A refusal leaves
	// the choice as it is.
	async function acquireTracks(replace: boolean): Promise<void> {
		const albumGroup = tracksGroup;
		if (!albumGroup) return;
		tracks = { pending: true, message: null, error: null, replaceVersions: replace };
		try {
			const answer = await acquire.mutateAsync({
				mbid: albumGroup,
				copyId,
				replaceVersions: replace
			});
			tracks = { pending: false, message: answer.message, error: null, replaceVersions: replace };
		} catch (error) {
			tracks = {
				pending: false,
				message: null,
				error: failureSentence(error, "The missing tracks couldn't be requested."),
				replaceVersions: replace
			};
		}
	}

	// "None of these is right": the selected release's album to keep the
	// files under, as its plan names it.
	const customAlbum = $derived.by(() => {
		if (!selected) return null;
		const plan = selectedIsCurrent ? currentPlan : selectedPlan;
		return plan?.release_mbid === selected ? plan.title : null;
	});

	async function keepOwnTrackList(): Promise<void> {
		const release = selected;
		if (!release || !status) return;
		noneRight = { done: null, error: null };
		try {
			await sealCustom.mutateAsync({
				userId: userId(),
				localId: copyId,
				rgMbid: group || undefined,
				releaseMbid: release,
				identityRevision: status.identity_revision
			});
			noneRight = { done: 'Your own track list is the edition now.', error: null };
		} catch (error) {
			noneRight = {
				done: null,
				error: failureSentence(error, "Your track list couldn't be kept.")
			};
		}
	}

	async function leaveAlone(): Promise<void> {
		if (!status) return;
		noneRight = { done: null, error: null };
		try {
			await leaveUnmanaged.mutateAsync({
				userId: userId(),
				localId: copyId,
				rgMbid: group || undefined,
				identityRevision: status.identity_revision
			});
			noneRight = { done: 'This album is left unmanaged now.', error: null };
		} catch (error) {
			noneRight = {
				done: null,
				error: failureSentence(error, "The album couldn't be left unmanaged.")
			};
		}
	}

	function finish(): void {
		open = false;
		onclose?.(movedTo);
	}

	const copyOptions = $derived(
		copies.map((item) => ({
			value: item.id,
			label: [item.title, count(item.track_count, 'track', 'tracks'), item.format?.toUpperCase()]
				.filter(Boolean)
				.join(' · ')
		}))
	);
	const tracksGroup = $derived(chosen?.choice.release_group_mbid ?? group);

	const pickTracksQuery = getEditionTracksQuery(
		userId,
		() => group,
		() => (request ? selected : null),
		() => open && Boolean(request)
	);
	const canPick = $derived(
		Boolean(request && !request.locked && selected && selected !== request.release)
	);

	async function pickEdition(): Promise<void> {
		const release = selected;
		if (!request || !release) return;
		actionError = null;
		try {
			await request.pick(release);
			finish();
		} catch (error) {
			actionError = failureSentence(error, "This edition couldn't be chosen.");
		}
	}
	const pickedName = $derived(currentPlan ? editionName(currentPlan.edition) : null);
</script>

<Modal
	bind:open
	size="sheet"
	title="Edition of {request?.albumTitle ?? copy?.title ?? 'this album'}"
	description={request ? 'Pick the release to get.' : SHEET_DESCRIPTION}
	onclose={finish}
>
	{#if request}
		{@render requestBody(request)}
	{:else}
		<div class="flex flex-col md:h-full md:min-h-0">
			{#if copies.length > 1 || steppedBack || reasonShown}
				<div class="flex flex-col gap-2 px-4 pt-3 md:px-6">
					{#if copies.length > 1}
						<div class="max-w-md">
							<SelectField label="Which copy?" bind:value={copyId} options={copyOptions} />
						</div>
					{/if}
					{#if steppedBack}
						<p class="alert alert-warning py-2 text-sm" role="note">{LEFT_UNMANAGED_NOTE}</p>
					{/if}
					{#if reasonShown}
						<p class="text-sm text-base-content/70">{reasonShown}</p>
					{/if}
				</div>
			{/if}

			<div
				class="grid flex-1 md:min-h-0 md:grid-cols-[minmax(0,2fr)_minmax(0,3fr)] md:divide-x md:divide-base-content/10"
			>
				<div
					class="flex-col gap-6 px-4 py-4 md:flex md:overflow-y-auto md:px-6 {step === 'detail'
						? 'hidden'
						: 'flex'}"
					aria-label="Editions"
					role="region"
				>
					<QueryBoundary query={statusQuery} what="the album's edition">
						{#snippet loading()}<EditionListSkeleton />{/snippet}
						<EditionList
							groups={visibleGroups}
							{selected}
							bind:filter
							showFilter={allRows.length > 6}
							onselect={select}
						/>
						{#if editionsQuery.isLoading}
							<EditionListSkeleton />
						{:else if editionsQuery.isError}
							<p class="text-sm text-base-content/60">
								MusicBrainz isn't answering, so the other editions can't be listed. Paste a release
								link below instead.
							</p>
						{/if}
						{#if searchQuery.isError}
							<p class="text-sm text-base-content/60" role="alert">
								{failureSentence(searchQuery.error, "MusicBrainz couldn't be searched.")}
							</p>
						{:else if searchArgs && searchQuery.data && searchQuery.data.items.length === 0}
							<p class="text-sm text-base-content/60">MusicBrainz found no releases for that.</p>
						{/if}
					</QueryBoundary>
					{#if canEdit}
						<EditionFind
							bind:title={findTitle}
							bind:artist={findArtist}
							searching={searchQuery.isFetching}
							onpaste={select}
							onsearch={() => (searchArgs = { title: findTitle.trim(), artist: findArtist.trim() })}
						/>
					{/if}
					{#if isAdmin && status}
						<EditionNoneRight
							album={customAlbum}
							canLeave={hold?.kind !== 'your_choice'}
							done={noneRight.done}
							error={noneRight.error}
							onkeep={keepOwnTrackList}
							onleave={leaveAlone}
						/>
					{/if}
					<div class="flex flex-wrap gap-2 md:hidden">
						{@render footerLinks()}
					</div>
				</div>

				<div
					class="flex-col gap-4 px-4 py-4 md:flex md:overflow-y-auto md:px-6 {step === 'list'
						? 'hidden'
						: 'flex'}"
					aria-label="Selected edition"
					role="region"
				>
					<Button
						variant="ghost"
						size="sm"
						icon={ChevronLeft}
						class="-ml-2 self-start md:hidden"
						onclick={() => (step = 'list')}>All editions</Button
					>
					{#if view === 'next' && chosen}
						<EditionNextSteps
							name={chosen.name}
							{status}
							plan={currentPlan?.release_mbid === chosen.release ? currentPlan : null}
							choice={chosen.choice}
							held={Boolean(hold)}
							error={actionError}
							onundo={undoChange}
							album={{ userId: userId(), localId: copyId, rgMbid: group || undefined }}
							group={tracksGroup}
							gaps={chosen.gaps}
							{sourceReady}
							{tracks}
							onacquire={acquireTracks}
						/>
					{:else if view === 'handedBack'}
						<div class="flex flex-col items-start gap-3 text-sm" aria-live="polite">
							{#if status?.identifying}
								<p class="flex items-center gap-2">
									<span class="loading loading-spinner loading-xs" aria-hidden="true"></span>
									Looking for the edition that fits your files best...
								</p>
							{:else if pickedName}
								<p>DroppedNeedle picked {pickedName}.</p>
								<AsyncButton variant="ghost" size="sm" onclick={undoChange}>Undo</AsyncButton>
							{:else if status}
								<p>{status.reason.message}</p>
							{/if}
							{#if actionError}<p class="text-error" role="alert">{actionError}</p>{/if}
						</div>
					{:else if selected}
						<EditionDetail
							release={selected}
							query={{
								data: selectedPlan,
								isPending: selectedPlanQuery.isPending,
								isError: selectedPlanQuery.isError,
								error: selectedPlanQuery.error,
								refetch: selectedPlanQuery.refetch
							}}
							isCurrent={selectedIsCurrent}
							{hiddenByFilter}
							albumHasGroup={Boolean(group)}
							{canEdit}
							primary={primaryKind ? primaryAction : undefined}
							{actionError}
							putBackCount={lastRemoval?.files ?? null}
							onremoveextras={moveExtras}
							onputback={putBack}
							{sourceReady}
							bind:getMissing
							bind:replaceVersions
							group={tracksGroup}
							{tracks}
							onacquire={acquireTracks}
							editionKind={status?.edition_kind ?? null}
							appPicked={status?.state === 'confirmed' || status?.state === 'unconfirmed'}
							onmatch={matchTrack}
							onunmatch={unmatchTrack}
						/>
					{:else if status}
						<p class="text-sm text-base-content/60">
							Pick an edition to compare it with your files.
						</p>
						{#if actionError}<p class="text-sm text-error" role="alert">{actionError}</p>{/if}
					{/if}
				</div>
			</div>
		</div>
	{/if}

	{#snippet actions()}
		{#if request}
			<div class="flex items-center gap-2">
				<div class="flex min-w-0 flex-1 flex-wrap items-center gap-2 md:hidden">
					{#if step === 'detail' && canPick}
						{@render pickAction()}
					{/if}
				</div>
				<Button size="sm" class="ml-auto" onclick={finish}>Done</Button>
			</div>
		{:else}
			<div class="flex items-center gap-2">
				<div class="hidden flex-1 flex-wrap gap-2 md:flex">
					{@render footerLinks()}
				</div>
				<div class="flex min-w-0 flex-1 flex-wrap items-center gap-2 md:hidden">
					{#if step === 'detail' && primaryKind}
						{@render primaryAction()}
					{/if}
				</div>
				<Button size="sm" onclick={finish}>Done</Button>
			</div>
		{/if}
	{/snippet}
</Modal>

{#snippet requestBody(pick: EditionRequestPick)}
	<div
		class="grid flex-1 md:h-full md:min-h-0 md:grid-cols-[minmax(0,2fr)_minmax(0,3fr)] md:divide-x md:divide-base-content/10"
	>
		<div
			class="flex-col gap-4 px-4 py-4 md:flex md:overflow-y-auto md:px-6 {step === 'detail'
				? 'hidden'
				: 'flex'}"
			aria-label="Editions"
			role="region"
		>
			<QueryBoundary query={editionsQuery} what="this album's editions">
				{#snippet loading()}<EditionListSkeleton />{/snippet}
				<EditionList
					groups={visibleGroups}
					{selected}
					bind:filter
					showFilter={allRows.length > 6}
					onselect={select}
					currentLabel={pick.requested ? 'Requested' : 'Getting'}
				/>
			</QueryBoundary>
		</div>

		<div
			class="flex-col gap-4 px-4 py-4 md:flex md:overflow-y-auto md:px-6 {step === 'list'
				? 'hidden'
				: 'flex'}"
			aria-label="Selected edition"
			role="region"
		>
			<Button
				variant="ghost"
				size="sm"
				icon={ChevronLeft}
				class="-ml-2 self-start md:hidden"
				onclick={() => (step = 'list')}>All editions</Button
			>
			{#if selected}
				{@const facts = factsOf(selected)}
				{@const link = outsideLink(`https://musicbrainz.org/release/${selected}`)}
				<header class="flex items-start gap-4">
					<AlbumImage
						customUrl={coverPath('release', selected, 250)}
						alt=""
						size="full"
						className="h-20 w-20 shrink-0 rounded-box"
						retryOnError={false}
					/>
					<div class="min-w-0">
						<h3 class="text-lg font-bold">{editionName(facts ?? {})}</h3>
						{#if facts}
							<p class="text-sm text-base-content/70">{editionFactsLine(facts)}</p>
						{/if}
						{#if link}
							<a
								class="link inline-flex items-center gap-1 text-xs text-base-content/55"
								href={link}
								target="_blank"
								rel="noreferrer"
								>Open in MusicBrainz <ExternalLink class="h-3 w-3" aria-hidden="true" /></a
							>
						{/if}
					</div>
				</header>
				<QueryBoundary query={pickTracksQuery} what="this edition's tracks">
					{#snippet loading()}<EditionDetailSkeleton />{/snippet}
					{#snippet children(info)}
						{@const discs = new Set(info.tracks.map((track) => track.disc_number)).size}
						<ol class="flex flex-col text-sm" aria-label="Tracks">
							{#each info.tracks as track (`${track.disc_number}-${track.position}`)}
								<li class="flex items-baseline gap-3 border-b border-base-content/5 py-1.5">
									<span class="w-8 shrink-0 text-right text-base-content/50 tabular-nums"
										>{discs > 1 ? `${track.disc_number}-${track.position}` : track.position}</span
									>
									<span class="min-w-0 flex-1 truncate">{track.title}</span>
									<span class="shrink-0 text-base-content/50 tabular-nums"
										>{formatDuration(track.length)}</span
									>
								</li>
							{/each}
						</ol>
					{/snippet}
				</QueryBoundary>
				{#if canPick}
					<div class="hidden flex-wrap items-center gap-3 md:flex">
						{@render pickAction()}
					</div>
				{/if}
				{#if actionError}<p class="text-sm text-error" role="alert">{actionError}</p>{/if}
			{/if}
		</div>
	</div>
{/snippet}

{#snippet pickAction()}
	<AsyncButton variant="primary" size="sm" onclick={pickEdition}
		>{request?.requested ? 'Switch to this edition' : 'Choose this edition'}</AsyncButton
	>
{/snippet}

{#snippet primaryAction()}
	{#if primaryKind === 'keep'}
		<AsyncButton variant="primary" size="sm" pending={confirm.isPending} onclick={keepEdition}
			>Keep this edition</AsyncButton
		>
	{:else if primaryKind === 'use'}
		{@const noneFit = selectedPlan ? fitsNoFile(selectedPlan) : false}
		<AsyncButton
			variant="primary"
			size="sm"
			pending={choose.isPending}
			pendingLabel="Matching your files..."
			disabled={!selectedPlan || noneFit}
			onclick={useEdition}>Use this edition</AsyncButton
		>
		{#if noneFit && selectedPlan}
			<span class="hidden text-sm text-base-content/65 md:inline"
				>{summarySentence(selectedPlan)}</span
			>
		{/if}
	{/if}
{/snippet}

{#snippet footerLinks()}
	{#if canEdit && letChooseLabel && view !== 'handedBack'}
		<AsyncButton variant="ghost" size="sm" pending={handBack.isPending} onclick={letChoose}
			>{letChooseLabel}</AsyncButton
		>
	{/if}
	{#if canEdit && status?.undo_available && view === 'compare'}
		<AsyncButton variant="ghost" size="sm" pending={undo.isPending} onclick={undoChange}
			>Undo last change</AsyncButton
		>
	{/if}
{/snippet}
