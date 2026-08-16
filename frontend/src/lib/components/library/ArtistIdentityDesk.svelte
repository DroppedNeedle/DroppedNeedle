<script lang="ts">
	import { resolve } from '$app/paths';
	import { goto } from '$app/navigation';
	import { page } from '$app/state';
	import {
		ArrowUpRight,
		BadgeCheck,
		CircleAlert,
		Fingerprint,
		ListFilter,
		Merge,
		Search,
		ShieldQuestion,
		Split,
		UsersRound
	} from 'lucide-svelte';
	import { onMount } from 'svelte';
	import { SvelteURLSearchParams } from 'svelte/reactivity';

	import {
		applyArtistMerge,
		previewArtistMerge
	} from '$lib/queries/library/LibraryCatalogMutations.svelte';
	import type { MembershipPreviewResponse } from '$lib/queries/library/LibraryOperationsTypes';
	import { dismissArtistDuplicateGroup } from '$lib/queries/artist-reconciliation/ArtistReconciliationMutations.svelte';
	import {
		getArtistDuplicateGroupQuery,
		getArtistDuplicateGroupsQuery,
		getArtistReconciliationProgressQuery
	} from '$lib/queries/artist-reconciliation/ArtistReconciliationQueries.svelte';
	import type {
		ArtistDuplicateGroupSummary,
		ArtistReconciliationGroupState
	} from '$lib/queries/artist-reconciliation/ArtistReconciliationTypes';
	import { authStore } from '$lib/stores/authStore.svelte';

	import ArtistIdentityDeskSkeleton from './ArtistIdentityDeskSkeleton.svelte';

	const stateOptions: Array<{ value: ArtistReconciliationGroupState | ''; label: string }> = [
		{ value: '', label: 'All groups' },
		{ value: 'waiting_for_identity', label: 'Waiting for identity' },
		{ value: 'provider_conflict', label: 'Provider conflict' },
		{ value: 'ambiguous_credit_structure', label: 'Ambiguous credit structure' },
		{ value: 'same_name_only', label: 'Same name only' },
		{ value: 'resolved_automatically', label: 'Resolved automatically' }
	];

	function parseState(value: string | null): ArtistReconciliationGroupState | '' {
		return stateOptions.some((option) => option.value === value)
			? (value as ArtistReconciliationGroupState)
			: '';
	}

	const appliedFilterState = $derived(parseState(page.url.searchParams.get('state')));
	const appliedSearch = $derived(page.url.searchParams.get('q') ?? '');
	const urlSelectedGroupId = $derived(page.url.searchParams.get('group'));
	let filterState = $state<ArtistReconciliationGroupState | ''>(
		parseState(page.url.searchParams.get('state'))
	);
	let search = $state(page.url.searchParams.get('q') ?? '');
	let selectedGroupId = $state<string | null>(page.url.searchParams.get('group'));
	let isWide = $state(false);
	let survivingId = $state('');
	let providerChoice = $state<'detach' | 'retain_survivor'>('retain_survivor');
	let confirmed = $state(false);
	let stalePreview = $state(false);
	let previewResult = $state<MembershipPreviewResponse | null>(null);
	let previewRequest = $state<{
		source_artist_ids: string[];
		surviving_artist_id: string;
		expected_revisions: Record<string, number>;
	} | null>(null);
	let mergeDialog: HTMLDialogElement;
	let mergeHeading: HTMLHeadingElement;
	let dismissDialog: HTMLDialogElement;
	let dismissHeading: HTMLHeadingElement;
	let actionOpener: HTMLButtonElement | null = null;

	const progressQuery = getArtistReconciliationProgressQuery(() => authStore.isAdmin);
	const groupsQuery = getArtistDuplicateGroupsQuery(() => ({
		state: appliedFilterState || undefined,
		search: appliedSearch.trim() || undefined
	}));
	const groups = $derived(groupsQuery.data?.pages.flatMap((response) => response.items) ?? []);
	const requestedArtistId = $derived(page.url.searchParams.get('artist'));
	const selectedSummary = $derived(
		groups.find((group) => group.id === selectedGroupId) ??
			groups.find((group) => group.members.some((member) => member.id === requestedArtistId)) ??
			groups[0] ??
			null
	);
	const detailQuery = getArtistDuplicateGroupQuery(() => selectedSummary?.id ?? null);
	const detail = $derived(detailQuery.data ?? null);
	const chosenSurvivor = $derived(
		detail?.members.find(
			(member) => member.id === (previewRequest?.surviving_artist_id ?? survivingId)
		) ?? null
	);
	const retiredMembers = $derived(
		detail?.members.filter(
			(member) => member.id !== (previewRequest?.surviving_artist_id ?? survivingId)
		) ?? []
	);
	const progressPercent = $derived(
		progressQuery.data?.expected_count
			? Math.min(
					100,
					Math.round((progressQuery.data.completed_count / progressQuery.data.expected_count) * 100)
				)
			: 0
	);

	$effect(() => {
		filterState = appliedFilterState;
		search = appliedSearch;
		selectedGroupId = urlSelectedGroupId;
	});

	$effect(() => {
		if (!selectedGroupId && selectedSummary) selectedGroupId = selectedSummary.id;
		if (detail && (!survivingId || !detail.members.some((member) => member.id === survivingId))) {
			survivingId = detail.recommended_survivor_id ?? detail.members[0]?.id ?? '';
		}
	});

	onMount(() => {
		const media = window.matchMedia('(min-width: 64rem)');
		const update = () => {
			isWide = media.matches;
		};
		update();
		media.addEventListener('change', update);
		return () => media.removeEventListener('change', update);
	});

	function stateLabel(state: ArtistReconciliationGroupState): string {
		return stateOptions.find((option) => option.value === state)?.label ?? state;
	}

	function stateClass(state: ArtistReconciliationGroupState): string {
		if (state === 'resolved_automatically') return 'badge-success';
		if (state === 'waiting_for_identity') return 'badge-info';
		if (state === 'same_name_only') return 'badge-ghost';
		return 'badge-warning';
	}

	function compactId(value: string): string {
		return value.length > 18 ? `${value.slice(0, 8)}…${value.slice(-6)}` : value;
	}

	function identityDeskHref(params: SvelteURLSearchParams): string {
		return params.size
			? resolve(`/library/management/artists?${params.toString()}`)
			: resolve('/library/management/artists');
	}

	function updateUrl(options: { clearGroup?: boolean; useDraftFilters?: boolean } = {}): void {
		const params = new SvelteURLSearchParams();
		const nextFilterState = options.useDraftFilters ? filterState : appliedFilterState;
		const nextSearch = options.useDraftFilters ? search.trim() : appliedSearch.trim();
		if (nextFilterState) params.set('state', nextFilterState);
		if (nextSearch) params.set('q', nextSearch);
		if (!options.clearGroup && selectedGroupId) params.set('group', selectedGroupId);
		void goto(identityDeskHref(params), {
			noScroll: true,
			keepFocus: true,
			replaceState: true
		});
	}

	function selectGroup(group: ArtistDuplicateGroupSummary): void {
		selectedGroupId = group.id;
		survivingId = group.recommended_survivor_id ?? group.members[0]?.id ?? '';
		providerChoice = 'retain_survivor';
		previewResult = null;
		previewRequest = null;
		stalePreview = false;
		confirmed = false;
		updateUrl();
	}

	function applyFilters(): void {
		selectedGroupId = null;
		providerChoice = 'retain_survivor';
		updateUrl({ clearGroup: true, useDraftFilters: true });
	}

	function mergeRequest() {
		if (!detail || !survivingId) return null;
		return {
			source_artist_ids: detail.members.map((member) => member.id),
			surviving_artist_id: survivingId,
			expected_revisions: detail.member_revisions
		};
	}

	async function openMerge(
		event: MouseEvent & { currentTarget: HTMLButtonElement }
	): Promise<void> {
		const request = mergeRequest();
		if (!request) return;
		actionOpener = event.currentTarget;
		providerChoice = 'retain_survivor';
		confirmed = false;
		stalePreview = false;
		previewResult = null;
		previewRequest = request;
		try {
			previewResult = await previewArtist.mutateAsync(request);
			mergeDialog.showModal();
			mergeHeading.focus();
		} catch {
			previewResult = null;
			previewRequest = null;
		}
	}

	async function mergeArtists(): Promise<void> {
		if (!previewRequest || !previewResult || !confirmed) return;
		try {
			await applyArtist.mutateAsync({
				...previewRequest,
				preview_token: previewResult.preview_token,
				provider_choice: providerChoice
			});
			mergeDialog.close();
			selectedGroupId = null;
			updateUrl({ clearGroup: true });
		} catch {
			confirmed = false;
			stalePreview = true;
			previewResult = null;
		}
	}

	function openDismiss(event: MouseEvent & { currentTarget: HTMLButtonElement }): void {
		actionOpener = event.currentTarget;
		dismissDialog.showModal();
		dismissHeading.focus();
	}

	async function dismissGroup(): Promise<void> {
		if (!detail) return;
		try {
			await dismissMutation.mutateAsync({
				groupId: detail.id,
				expectedMemberRevisions: detail.member_revisions
			});
			dismissDialog.close();
			selectedGroupId = null;
			updateUrl({ clearGroup: true });
		} catch {
			return;
		}
	}

	const previewArtist = previewArtistMerge();
	const applyArtist = applyArtistMerge();
	const dismissMutation = dismissArtistDuplicateGroup();
</script>

<section class="space-y-5" aria-labelledby="artist-identity-desk-title">
	<header class="flex flex-wrap items-end justify-between gap-4">
		<div>
			<p class="font-mono text-xs uppercase tracking-[0.18em] text-library-identify/80">
				Provider proof desk
			</p>
			<h1 id="artist-identity-desk-title" class="font-display text-3xl font-bold">
				Artist identity
			</h1>
			<p class="mt-1 max-w-3xl text-sm text-base-content/60">
				MusicBrainz release and track credits can prove identity. Similar names alone never merge
				artists.
			</p>
		</div>
		<a href={resolve('/library/management?tab=scanning')} class="btn btn-ghost btn-sm">
			Back to Scan &amp; identify
		</a>
	</header>

	{#if progressQuery.isLoading}
		<div class="skeleton h-32 rounded-box"></div>
	{:else if progressQuery.isError}
		<div class="alert alert-error">Could not load artist reconciliation progress.</div>
	{:else if progressQuery.data}
		<section
			class="rounded-box border border-base-content/10 bg-base-100 p-4"
			aria-label="Reconciliation progress"
		>
			<div class="flex flex-wrap items-center justify-between gap-3">
				<div>
					<p class="text-xs font-semibold uppercase tracking-wide text-base-content/50">
						Background reconciliation
					</p>
					<p class="mt-1 font-semibold">
						{progressQuery.data.state.replaceAll('_', ' ')} · {progressQuery.data.completed_count.toLocaleString()}
						of {progressQuery.data.expected_count.toLocaleString()} albums
					</p>
				</div>
				<span class="font-mono text-sm text-base-content/60">{progressPercent}%</span>
			</div>
			<progress class="progress progress-primary mt-3 w-full" value={progressPercent} max="100"
			></progress>
			<div class="mt-4 grid grid-cols-2 gap-3 text-sm sm:grid-cols-4">
				<div>
					<span class="block text-xs text-base-content/50">Resolved automatically</span><strong
						>{progressQuery.data.automatically_resolved_count.toLocaleString()}</strong
					>
				</div>
				<div>
					<span class="block text-xs text-base-content/50">Waiting for identity</span><strong
						>{progressQuery.data.waiting_for_identity_count.toLocaleString()}</strong
					>
				</div>
				<div>
					<span class="block text-xs text-base-content/50">Needs judgement</span><strong
						>{progressQuery.data.genuine_review_count.toLocaleString()}</strong
					>
				</div>
				<div>
					<span class="block text-xs text-base-content/50">Provider conflicts</span><strong
						>{progressQuery.data.provider_conflict_count.toLocaleString()}</strong
					>
				</div>
			</div>
		</section>
	{/if}

	<form
		class="flex flex-wrap gap-2 rounded-box border border-base-content/10 bg-base-100 p-3"
		onsubmit={(event) => {
			event.preventDefault();
			applyFilters();
		}}
	>
		<label class="input input-bordered flex min-w-64 flex-1 items-center gap-2">
			<Search class="h-4 w-4 text-base-content/45" />
			<input
				bind:value={search}
				aria-label="Search artist groups"
				placeholder="Search names or local artists"
			/>
		</label>
		<label class="select select-bordered flex items-center gap-2">
			<ListFilter class="h-4 w-4 text-base-content/45" />
			<select bind:value={filterState} aria-label="Evidence state">
				{#each stateOptions as option (option.value)}
					<option value={option.value}>{option.label}</option>
				{/each}
			</select>
		</label>
		<button class="btn btn-primary" type="submit">Apply filters</button>
	</form>

	{#if groupsQuery.isLoading}
		<ArtistIdentityDeskSkeleton />
	{:else if groupsQuery.isError}
		<div class="alert alert-error">Could not load artist identity groups.</div>
	{:else if !groups.length}
		<div
			class="rounded-box border border-dashed border-base-content/20 bg-base-100 px-6 py-14 text-center"
		>
			<BadgeCheck class="mx-auto h-10 w-10 text-success" />
			<h2 class="mt-3 font-display text-xl font-semibold">No matching artist groups</h2>
			<p class="mx-auto mt-1 max-w-lg text-sm text-base-content/55">
				Try changing the filters. Artists remain separate until provider evidence proves they match
				or an administrator confirms a merge.
			</p>
		</div>
	{:else}
		<div class="grid items-start gap-5 lg:grid-cols-[minmax(0,1fr)_23rem]">
			<div class="space-y-3" aria-label="Artist identity dossiers">
				{#each groups as group (group.id)}
					<button
						type="button"
						class="w-full rounded-box border bg-base-100 p-4 text-left hover:bg-base-200 focus-visible:outline-2 focus-visible:outline-primary {selectedSummary?.id ===
						group.id
							? 'border-primary/50'
							: 'border-base-content/10'}"
						onclick={() => selectGroup(group)}
					>
						<div class="flex flex-wrap items-start justify-between gap-3">
							<div class="min-w-0">
								<p class="font-display text-lg font-semibold">{group.display_name}</p>
								<p class="mt-1 text-sm text-base-content/55">
									{group.member_count} local records · {group.affected_reference_count.toLocaleString()}
									references
								</p>
							</div>
							<span class="badge {stateClass(group.state)}">{stateLabel(group.state)}</span>
						</div>
						<div class="mt-3 flex flex-wrap gap-2">
							{#each group.members as member (member.id)}
								<span class="rounded-lg bg-base-200 px-2 py-1 font-mono text-xs">
									{member.name} · {compactId(member.id)}
								</span>
							{/each}
						</div>
					</button>
					{#if !isWide && selectedSummary?.id === group.id}
						<div class="pt-2">
							{@render identityInspector()}
						</div>
					{/if}
				{/each}
				{#if groupsQuery.hasNextPage}
					<button
						class="btn btn-outline w-full"
						disabled={groupsQuery.isFetchingNextPage}
						onclick={() => void groupsQuery.fetchNextPage()}
					>
						{groupsQuery.isFetchingNextPage ? 'Loading…' : 'Load more groups'}
					</button>
				{/if}
			</div>

			{#snippet identityInspector()}
				<aside class={isWide ? 'sticky top-24' : ''} aria-label="Artist identity inspector">
					{#if !selectedSummary}
						<div class="rounded-box border border-base-content/10 bg-base-100 p-6 text-center">
							<Fingerprint class="mx-auto h-8 w-8 text-base-content/35" />
							<p class="mt-3 font-semibold">Select a dossier</p>
							<p class="mt-1 text-sm text-base-content/55">
								Inspect evidence and persisted references here.
							</p>
						</div>
					{:else if detailQuery.isLoading}
						<div class="skeleton h-96 rounded-box"></div>
					{:else if detailQuery.isError || !detail}
						<div class="alert alert-error">Could not load this artist dossier.</div>
					{:else}
						<div
							class="rounded-box border border-base-content/10 bg-base-100 {isWide
								? 'max-h-[calc(100vh-8rem)] overflow-y-auto'
								: ''}"
						>
							<header class="border-b border-base-content/10 p-4">
								<div class="flex items-start justify-between gap-3">
									<div>
										<p class="font-mono text-xs uppercase tracking-wide text-base-content/45">
											Evidence dossier
										</p>
										<h2 class="mt-1 font-display text-xl font-bold">{detail.display_name}</h2>
									</div>
									<span class="badge {stateClass(detail.state)}">{stateLabel(detail.state)}</span>
								</div>
								<p class="mt-2 text-sm text-base-content/60">
									{detail.reason_code.replaceAll('_', ' ')}
								</p>
							</header>

							<div class="space-y-5 p-4">
								<section aria-labelledby="local-records-title">
									<h3 id="local-records-title" class="flex items-center gap-2 font-semibold">
										<UsersRound class="h-4 w-4" /> Local records
									</h3>
									<div class="mt-2 space-y-2">
										{#each detail.members as member (member.id)}
											<label class="block rounded-lg border border-base-content/10 p-3">
												<span class="flex items-start gap-2">
													{#if detail.state !== 'resolved_automatically'}
														<input
															type="radio"
															class="radio radio-sm mt-0.5"
															bind:group={survivingId}
															value={member.id}
														/>
													{/if}
													<span
														><strong>{member.name}</strong><span
															class="block break-all font-mono text-xs text-base-content/50"
															>{member.id} · revision {member.row_revision}</span
														></span
													>
												</span>
												<span class="mt-2 block text-xs text-base-content/60">
													{member.album_credit_count} album · {member.track_credit_count} track credits
													· {member.proven_credit_count}/{member.active_credit_count} proven
												</span>
												{#if member.provider_mbid}<span
														class="mt-1 block break-all font-mono text-xs text-primary"
														>MusicBrainz {member.provider_mbid}</span
													>{/if}
											</label>
										{/each}
									</div>
								</section>

								<section aria-labelledby="references-title">
									<h3 id="references-title" class="font-semibold">References that would move</h3>
									<dl class="mt-2 grid grid-cols-2 gap-2 text-sm">
										{#each Object.entries(detail.reference_counts) as [kind, count] (kind)}
											<div class="rounded-lg bg-base-200 p-2">
												<dt class="text-xs text-base-content/50">{kind.replaceAll('_', ' ')}</dt>
												<dd class="font-semibold">{count}</dd>
											</div>
										{/each}
									</dl>
								</section>

								<section aria-labelledby="provider-evidence-title">
									<h3 id="provider-evidence-title" class="flex items-center gap-2 font-semibold">
										<Fingerprint class="h-4 w-4" /> Exact provider evidence
									</h3>
									{#if detail.evidence.length}
										<ul class="mt-2 space-y-2 text-sm">
											{#each detail.evidence as item (`${item.subject_kind}:${item.subject_id}:${item.artist_mbid}`)}
												<li class="rounded-lg bg-base-200 p-2">
													<strong>{item.subject_name}</strong>
													<span class="block text-xs text-base-content/55"
														>{item.subject_kind} credit “{item.credited_name}{item.join_phrase}”</span
													>
													<span class="block break-all font-mono text-xs text-primary"
														>{item.artist_mbid}</span
													>
												</li>
											{/each}
										</ul>
									{:else}
										<p class="mt-2 flex gap-2 rounded-lg bg-warning/10 p-3 text-sm">
											<ShieldQuestion class="h-4 w-4 shrink-0" /> No complete provider proof is stored
											for this group.
										</p>
									{/if}
								</section>

								{#if detail.releases.length}
									<section aria-labelledby="release-work-title">
										<h3 id="release-work-title" class="font-semibold">Release identity work</h3>
										<ul class="mt-2 space-y-2 text-sm">
											{#each detail.releases as release (release.id)}
												<li
													class="flex items-center justify-between gap-2 rounded-lg border border-base-content/10 p-2"
												>
													<span
														>{release.name}<span class="block text-xs text-base-content/50"
															>{release.identity_ready
																? release.exact_track_mapping_ready
																	? 'Exact release and tracks accepted'
																	: 'Needs exact track mapping'
																: 'Needs release identification'}</span
														></span
													>
													<a
														class="btn btn-ghost btn-xs"
														href={`/album/${release.id}`}
														aria-label={`Open ${release.name}`}
														><ArrowUpRight class="h-3.5 w-3.5" /></a
													>
												</li>
											{/each}
										</ul>
									</section>
								{/if}

								{#if detail.state !== 'resolved_automatically'}
									<div class="grid gap-2">
										<button
											class="btn btn-primary"
											disabled={!survivingId || previewArtist.isPending}
											onclick={(event) => void openMerge(event)}
											><Merge class="h-4 w-4" /> Preview group merge</button
										>
										<button
											class="btn btn-ghost"
											disabled={dismissMutation.isPending}
											onclick={openDismiss}
											><Split class="h-4 w-4" /> Mark records as distinct</button
										>
									</div>
								{/if}
							</div>
						</div>
					{/if}
				</aside>
			{/snippet}

			{#if isWide}
				{@render identityInspector()}
			{/if}
		</div>
	{/if}
</section>

<dialog
	bind:this={mergeDialog}
	class="modal"
	aria-labelledby="artist-group-merge-title"
	onclose={() => actionOpener?.focus()}
>
	<div class="modal-box max-w-2xl">
		<h2
			bind:this={mergeHeading}
			id="artist-group-merge-title"
			tabindex="-1"
			class="font-display text-xl font-bold"
		>
			Confirm artist group merge
		</h2>
		{#if stalePreview}
			<div class="alert alert-warning mt-4">
				<CircleAlert class="h-4 w-4" /> The records changed after preview. Close this dialog and create
				a fresh preview.
			</div>
		{:else if previewResult}
			<p class="mt-2 text-sm text-base-content/65">
				{previewResult.aliases.length} retired IDs will remain aliases. Files, tags, artwork, and paths
				are untouched.
			</p>
			<div class="mt-4 grid gap-3 sm:grid-cols-2" aria-label="Artist merge identities">
				<div class="rounded-box border border-primary/25 bg-primary/5 p-3">
					<p class="text-xs font-semibold uppercase tracking-wide text-primary">Chosen survivor</p>
					{#if chosenSurvivor}
						<p class="mt-1 font-semibold">{chosenSurvivor.name}</p>
						<p class="break-all font-mono text-xs text-base-content/55">
							{chosenSurvivor.id}
						</p>
						{#if chosenSurvivor.provider_mbid}
							<p class="mt-1 break-all font-mono text-xs text-primary">
								MusicBrainz {chosenSurvivor.provider_mbid}
							</p>
						{/if}
					{/if}
				</div>
				<div class="rounded-box border border-base-content/10 bg-base-200/60 p-3">
					<p class="text-xs font-semibold uppercase tracking-wide text-base-content/50">
						Retired records · {retiredMembers.length}
					</p>
					<ul class="mt-1 space-y-1">
						{#each retiredMembers as member (member.id)}
							<li>
								<span class="font-semibold">{member.name}</span>
								<span class="block break-all font-mono text-xs text-base-content/55">
									{member.id}
								</span>
							</li>
						{/each}
					</ul>
				</div>
			</div>
			<dl class="mt-4 grid grid-cols-2 gap-2 text-sm sm:grid-cols-3">
				{#each Object.entries(previewResult.reference_counts) as [kind, count] (kind)}
					<div class="rounded-lg bg-base-200 p-2">
						<dt class="text-xs text-base-content/50">{kind.replaceAll('_', ' ')}</dt>
						<dd class="font-semibold">{count}</dd>
					</div>
				{/each}
			</dl>
			{#if previewResult.identity_conflicts.length}
				<fieldset class="mt-4 rounded-box border border-warning/30 p-3">
					<legend class="px-1 font-semibold">Provider identity conflict</legend>
					<label class="mt-2 flex gap-2 text-sm"
						><input
							type="radio"
							class="radio radio-sm"
							bind:group={providerChoice}
							value="retain_survivor"
						/> Keep the chosen survivor's provider identity</label
					>
					<label class="mt-2 flex gap-2 text-sm"
						><input
							type="radio"
							class="radio radio-sm"
							bind:group={providerChoice}
							value="detach"
						/> Detach conflicting provider identities</label
					>
				</fieldset>
			{/if}
			<label class="mt-4 flex items-start gap-2 text-sm"
				><input type="checkbox" class="checkbox checkbox-sm mt-0.5" bind:checked={confirmed} /> Merge
				all records in this dossier and preserve retired IDs as aliases.</label
			>
		{/if}
		<div class="modal-action">
			<button class="btn btn-ghost" onclick={() => mergeDialog.close()}>Cancel</button>
			<button
				class="btn btn-primary"
				disabled={!previewResult || !confirmed || applyArtist.isPending}
				onclick={() => void mergeArtists()}>Merge artists</button
			>
		</div>
	</div>
	<form method="dialog" class="modal-backdrop">
		<button aria-label="Close merge confirmation">close</button>
	</form>
</dialog>

<dialog
	bind:this={dismissDialog}
	class="modal"
	aria-labelledby="artist-group-dismiss-title"
	onclose={() => actionOpener?.focus()}
>
	<div class="modal-box max-w-md">
		<h2
			bind:this={dismissHeading}
			id="artist-group-dismiss-title"
			tabindex="-1"
			class="font-display text-xl font-bold"
		>
			Keep these artists distinct?
		</h2>
		<p class="mt-3 text-sm text-base-content/65">
			This dismisses the current record revisions as a candidate group. If any of these records
			changes, the evidence can be reviewed again.
		</p>
		<div class="modal-action">
			<button class="btn btn-ghost" onclick={() => dismissDialog.close()}>Cancel</button>
			<button
				class="btn btn-primary"
				disabled={dismissMutation.isPending}
				onclick={() => void dismissGroup()}>Mark as distinct</button
			>
		</div>
	</div>
	<form method="dialog" class="modal-backdrop">
		<button aria-label="Close distinct confirmation">close</button>
	</form>
</dialog>
