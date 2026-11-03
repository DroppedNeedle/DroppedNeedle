<!--
	The edition plate on a library album's page: which edition the copy is,
	who picked it, how many of its tracks the files hold and whether its
	folder keeps its own tags. Under it, for everyone, why a best guess or a
	missing match is what it is; for curators, the review waiting on them,
	and for everyone else a "Looks wrong" button. For curators the plate
	opens the edition sheet, as `?edition=choose` in the address does; with
	`from` (Check albums or Downloads) the sheet goes back there when it
	closes. With `request` and no copies it names the edition a request
	gets, and opens the sheet to pick another until the download starts.
-->
<script lang="ts">
	import { goto } from '$app/navigation';
	import { page } from '$app/state';
	import { ChevronRight, Flag, Hand } from 'lucide-svelte';
	import { authStore } from '$lib/stores/authStore.svelte';
	import { toastStore } from '$lib/stores/toast';
	import Button from '$lib/ui/Button.svelte';
	import { withBasePath } from '$lib/utils/basePath';
	import { createImportHeldMutation } from '$lib/features/downloads/queries/HeldQueries.svelte';
	import { getLibraryAlbumDetailQuery } from '$lib/features/library/queries/LibraryQueries.svelte';
	import type { AlbumView } from '$lib/types';
	import {
		createMarkLooksWrongMutation,
		getAlbumEditionStatusQuery,
		getAlbumEditionsQuery,
		getEditionPlanQuery
	} from '$lib/features/library/queries/EditionQueries.svelte';
	import { albumHref } from '$lib/utils/entityRoutes';
	import EditionDisc from './EditionDisc.svelte';
	import EditionSheet from './EditionSheet.svelte';
	import {
		editionFactsLine,
		editionName,
		failureSentence,
		moreCopies,
		plateCopy,
		type PlateCopy
	} from './editionCopy';
	import type { EditionRequestPick } from './editionSheetState.svelte';

	interface Props {
		/** The album's library copies; the plate shows the first. */
		copies: AlbumView[];
		/** The release group the page shows, empty on a local album's page. */
		groupMbid?: string;
		/** The edition a request gets, on the page of an album with no copies. */
		request?: EditionRequestPick;
	}

	let { copies, groupMbid = '', request }: Props = $props();

	const userId = () => authStore.user?.id;
	const copy = $derived(copies[0]);
	const statusQuery = getAlbumEditionStatusQuery(userId, () => copy?.id ?? '');
	const status = $derived(statusQuery.data?.album_id === copy?.id ? statusQuery.data : undefined);
	// The same plan, under the same key, as the sheet's current edition, so
	// the page and the sheet never disagree.
	const planQuery = getEditionPlanQuery(
		userId,
		() => copy?.id ?? '',
		() => null,
		() => status?.edition_kind === 'release'
	);
	const plan = $derived(
		planQuery.data && planQuery.data.release_mbid === status?.release_mbid ? planQuery.data : null
	);
	const lines = $derived(status ? plateCopy(status, plan, copy?.track_count ?? 0) : null);
	const canEdit = $derived(authStore.isTrusted);

	// No copies: the edition a request gets, named from the album's editions.
	const wanted = $derived(!copy && request ? request : null);
	const editionsQuery = getAlbumEditionsQuery(
		userId,
		() => groupMbid,
		() => Boolean(wanted)
	);
	const wantedEdition = $derived(
		wanted?.release
			? (editionsQuery.data?.items.find((item) => item.release_mbid === wanted.release) ?? null)
			: null
	);
	const wantedLines = $derived.by(() => {
		if (!wanted) return null;
		const facts = wantedEdition
			? {
					title: wantedEdition.title,
					disambiguation: wantedEdition.disambiguation,
					date: wantedEdition.date,
					country: wantedEdition.country,
					formats: wantedEdition.media_formats,
					track_count: wantedEdition.track_count
				}
			: null;
		const name = facts ? editionName(facts) : 'Edition';
		const state = wanted.locked ? 'Downloading' : wanted.requested ? 'Requested' : 'To get';
		return {
			name,
			state,
			facts: facts ? editionFactsLine(facts) : '',
			canChange: canEdit && !wanted.locked,
			label: `${state}: ${name}.${canEdit && !wanted.locked ? ' Change edition' : ''}`
		};
	});

	let sheetOpen = $state(false);
	let sheetMounted = $state(false);

	let sheetCopies = $state<AlbumView[]>([]);

	// The sheet keeps the copies it opened with: a choice can move the copy
	// off this page while the sheet still shows what happened.
	function openSheet(): void {
		sheetCopies = copies;
		sheetMounted = true;
		sheetOpen = true;
	}

	// Where the sheet goes back to when it closes: a Check albums list at
	// the next album, or Downloads, where a held better copy waits.
	const LISTS = ['unconfirmed', 'unmatched', 'looks_wrong'];
	let back = $state<{ label: string; href: string } | null>(null);
	let heldRetry = $state<{ entryId: number; fileIds: number[] } | null>(null);

	function readReturn(params: URLSearchParams): void {
		const from = params.get('from') ?? '';
		if (LISTS.includes(from)) {
			const next = params.get('next');
			const query = new URLSearchParams([
				...(from !== 'unconfirmed' ? [['view', from]] : []),
				...(next ? [['focus', next]] : [])
			]).toString();
			back = { label: 'Back to Check albums', href: `/library/review${query ? `?${query}` : ''}` };
		} else if (from === 'downloads') {
			back = { label: 'Back to Downloads', href: '/downloads' };
			const entryId = Number(params.get('held'));
			const fileIds = (params.get('files') ?? '')
				.split(',')
				.map(Number)
				.filter((id) => Number.isInteger(id) && id > 0);
			heldRetry =
				Number.isInteger(entryId) && entryId > 0 && fileIds.length ? { entryId, fileIds } : null;
		}
	}

	let askedToChoose = false;
	$effect(() => {
		if (askedToChoose || !canEdit || !copy) return;
		if (page.url.searchParams.get('edition') !== 'choose') return;
		askedToChoose = true;
		readReturn(page.url.searchParams);
		openSheet();
		// Taken from the address once used, so a reload or a later redirect
		// does not open the sheet again.
		const url = new URL(page.url);
		for (const key of ['edition', 'from', 'next', 'held', 'files']) url.searchParams.delete(key);
		void goto(url, { replaceState: true, noScroll: true, keepFocus: true });
	});

	// A better copy held for this album's check goes in as soon as a person
	// settles the edition, instead of at the held list's next pass.
	const importHeld = createImportHeldMutation();
	function settled(): void {
		const retry = heldRetry;
		if (!retry) return;
		heldRetry = null;
		importHeld.mutate({ entryId: retry.entryId, fileIds: retry.fileIds });
	}

	function closed(movedTo: string | null): void {
		const to = back;
		back = null;
		heldRetry = null;
		if (to) void goto(withBasePath(to.href));
		else if (movedTo) void goto(albumHref(movedTo));
	}

	// "Looks wrong": anyone who cannot change the edition can say so.
	const markWrong = createMarkLooksWrongMutation();
	async function toggleLooksWrong(): Promise<void> {
		if (!copy || !status) return;
		const looksWrong = !status.looks_wrong;
		try {
			await markWrong.mutateAsync({ userId: userId(), localId: copy.id, looksWrong });
			toastStore.show({
				message: looksWrong ? 'Thanks. A curator will check it.' : 'Mark taken back.',
				type: 'success'
			});
		} catch (error) {
			toastStore.show({
				message: failureSentence(error, "The mark couldn't be saved."),
				type: 'error'
			});
		}
	}

	// Why a guess, or no match, is what it is: shown to everyone.
	const reason = $derived(
		status && (status.state === 'unconfirmed' || status.state === 'unmatched')
			? status.reason.message
			: null
	);
	// The open review of the copy, from its library detail.
	const detailQuery = getLibraryAlbumDetailQuery(() => (canEdit ? (copy?.id ?? '') : ''));
	const reviewId = $derived(
		detailQuery.data?.id === copy?.id ? (detailQuery.data?.pending_review_id ?? null) : null
	);
</script>

{#if wanted && wantedLines}
	{#if editionsQuery.isPending && wanted.release}
		<div
			class="flex w-full max-w-xl items-center gap-3 rounded-box border border-base-content/10 p-3"
			aria-hidden="true"
		>
			<div class="skeleton h-10 w-10 shrink-0 rounded-full"></div>
			<div class="flex-1 space-y-1.5">
				<div class="skeleton h-4 w-1/2"></div>
				<div class="skeleton h-3 w-3/4"></div>
			</div>
		</div>
	{:else if wantedLines.canChange}
		<button
			type="button"
			class="flex w-full max-w-xl items-center gap-3 rounded-box border border-base-content/10 bg-base-100/40 p-3 text-left backdrop-blur-sm transition-colors hover:border-primary/40 focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-primary motion-reduce:transition-none"
			aria-label={wantedLines.label}
			onclick={openSheet}
		>
			{@render wantedBody(wantedLines)}
		</button>
	{:else}
		<div
			class="flex w-full max-w-xl items-center gap-3 rounded-box border border-base-content/10 bg-base-100/40 p-3 backdrop-blur-sm"
			role="group"
			aria-label={wantedLines.label}
		>
			{@render wantedBody(wantedLines)}
		</div>
	{/if}
{:else if !copy}
	<!-- The copy left this page; only the sheet stays until it closes. -->
{:else if statusQuery.isPending && !status}
	<div
		class="flex w-full max-w-xl items-center gap-3 rounded-box border border-base-content/10 p-3"
		aria-hidden="true"
	>
		<div class="skeleton h-10 w-10 shrink-0 rounded-full"></div>
		<div class="flex-1 space-y-1.5">
			<div class="skeleton h-4 w-1/2"></div>
			<div class="skeleton h-3 w-3/4"></div>
		</div>
	</div>
{:else if statusQuery.isError && !status}
	<p class="flex items-center gap-2 text-sm text-base-content/60">
		The album's edition couldn't be loaded.
		<Button variant="ghost" size="xs" onclick={() => statusQuery.refetch()}>Retry</Button>
	</p>
{:else if lines}
	{@render plate(lines)}
	{#if reason || (canEdit && reviewId) || (!canEdit && status)}
		<div class="mt-2 flex max-w-xl flex-wrap items-center gap-x-3 gap-y-1 text-sm">
			{#if reason}
				<p class="text-base-content/70">{reason}</p>
			{/if}
			{#if canEdit && reviewId}
				<a
					class="link link-warning text-sm"
					href={withBasePath(`/library/review?review=${encodeURIComponent(reviewId)}`)}
					>Needs you: open its review</a
				>
			{/if}
			{#if !canEdit && status}
				<Button
					variant="ghost"
					size="xs"
					icon={Flag}
					aria-pressed={status.looks_wrong}
					loading={markWrong.isPending}
					onclick={() => void toggleLooksWrong()}
					>{status.looks_wrong ? 'Marked as wrong' : 'Looks wrong'}</Button
				>
			{/if}
		</div>
	{/if}
{/if}

{#snippet plate(plateLines: PlateCopy)}
	{#if canEdit}
		<button
			type="button"
			class="flex w-full max-w-xl items-center gap-3 rounded-box border border-base-content/10 bg-base-100/40 p-3 text-left backdrop-blur-sm transition-colors hover:border-primary/40 focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-primary motion-reduce:transition-none"
			aria-label={plateLines.label}
			onclick={openSheet}
		>
			{@render body(plateLines)}
		</button>
	{:else}
		<div
			class="flex w-full max-w-xl items-center gap-3 rounded-box border border-base-content/10 bg-base-100/40 p-3 backdrop-blur-sm"
			role="group"
			aria-label={plateLines.label.replace(/ (Change|Choose) edition$/, '')}
		>
			{@render body(plateLines)}
		</div>
	{/if}
{/snippet}

{#snippet wantedBody(wantedCopy: NonNullable<typeof wantedLines>)}
	<EditionDisc fill={0} tone={wanted?.locked ? 'busy' : 'none'} spinning={false} />
	<span class="min-w-0 flex-1">
		<span class="flex flex-wrap items-center gap-x-2 gap-y-0.5">
			<span class="font-semibold">{wantedCopy.name}</span>
			<span class="text-xs font-medium text-base-content/70">{wantedCopy.state}</span>
		</span>
		{#if wantedCopy.facts}
			<span class="block text-xs text-base-content/65">{wantedCopy.facts}</span>
		{/if}
	</span>
	{#if wantedCopy.canChange}
		<span class="flex shrink-0 items-center gap-0.5 self-end text-sm font-medium text-primary">
			Change<ChevronRight class="h-4 w-4" aria-hidden="true" />
		</span>
	{/if}
{/snippet}

{#snippet body(copyLines: NonNullable<typeof lines>)}
	<EditionDisc {...copyLines.disc} />
	<span class="min-w-0 flex-1">
		<span class="flex flex-wrap items-center gap-x-2 gap-y-0.5">
			<span class="font-semibold">{copyLines.line1}</span>
			{#if copyLines.chip}
				<span
					class="inline-flex items-center gap-1 text-xs font-medium {copyLines.chip.kind === 'guess'
						? 'text-warning'
						: 'text-base-content/70'}"
				>
					{#if copyLines.chip.kind === 'yours'}<Hand class="h-3 w-3" aria-hidden="true" />{/if}
					{copyLines.chip.label}
				</span>
			{/if}
		</span>
		<span class="block text-xs text-base-content/65">
			{copyLines.line2}{#each copyLines.warnings as warning (warning)}<span class="text-warning"
					>{` · ${warning}`}</span
				>{/each}
		</span>
		{#if status?.keeps_own_tags}
			<span class="block text-xs text-base-content/55">Folder keeps its own tags</span>
		{/if}
		{#if copies.length > 1}
			<span class="block text-xs text-base-content/55">{moreCopies(copies.length - 1)}</span>
		{/if}
	</span>
	{#if canEdit}
		<span class="flex shrink-0 items-center gap-0.5 self-end text-sm font-medium text-primary">
			{copyLines.action}<ChevronRight class="h-4 w-4" aria-hidden="true" />
		</span>
	{/if}
{/snippet}

{#if sheetMounted && wanted}
	<EditionSheet bind:open={sheetOpen} copies={[]} {groupMbid} request={wanted} />
{:else if sheetMounted && sheetCopies.length}
	<EditionSheet
		bind:open={sheetOpen}
		copies={sheetCopies}
		{groupMbid}
		onclose={closed}
		backLabel={back?.label ?? null}
		onsettled={settled}
	/>
{/if}
