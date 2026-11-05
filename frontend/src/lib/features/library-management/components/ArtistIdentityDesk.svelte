<script lang="ts">
	import { goto } from '$app/navigation';
	import { page } from '$app/state';
	import {
		ArrowUpRight,
		BadgeCheck,
		CircleAlert,
		FingerprintPattern,
		ListFilter,
		Merge,
		Search,
		ShieldQuestionMark,
		Split,
		Undo2,
		UsersRound
	} from 'lucide-svelte';
	import { onMount } from 'svelte';
	import { SvelteURLSearchParams } from 'svelte/reactivity';

	import {
		createApplyArtistMergeMutation,
		createPreviewArtistMergeMutation
	} from '$lib/features/library/queries/LibraryCatalogMutations.svelte';
	import type { ArtistMergePreviewResponse } from '$lib/features/library-management/queries/LibraryOperationsTypes';
	import {
		createDismissArtistDuplicateGroupMutation,
		createUndoArtistJoinMutation
	} from '$lib/features/library/queries/artist-reconciliation.svelte';
	import {
		getArtistDuplicateGroupQuery,
		getArtistDuplicateGroupsQuery,
		getArtistReconciliationProgressQuery
	} from '$lib/features/library/queries/artist-reconciliation.svelte';
	import type {
		ArtistDuplicateGroupSummary,
		ArtistReconciliationGroupState
	} from '$lib/features/library/queries/artist-reconciliation.svelte';
	import { authStore } from '$lib/stores/authStore.svelte';
	import { withBasePath } from '$lib/utils/basePath';
	import Button from '$lib/ui/Button.svelte';
	import ConfirmDialog from '$lib/ui/ConfirmDialog.svelte';
	import Modal from '$lib/ui/Modal.svelte';
	import Card from '$lib/ui/Card.svelte';
	import IconButton from '$lib/ui/IconButton.svelte';
	import Input from '$lib/ui/Input.svelte';

	import ArtistIdentityDeskSkeleton from './ArtistIdentityDeskSkeleton.svelte';

	const stateOptions: Array<{ value: ArtistReconciliationGroupState | ''; label: string }> = [
		{ value: '', label: 'All groups' },
		{ value: 'waiting_for_identity', label: 'Waiting for identity' },
		{ value: 'provider_conflict', label: 'Provider conflict' },
		{ value: 'ambiguous_credit_structure', label: 'Ambiguous credit structure' },
		{ value: 'same_name_only', label: 'Same name only' },
		{ value: 'credits_moved', label: 'Songs moved' },
		{ value: 'shared_provider_identity', label: 'Same MusicBrainz artist' },
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
	let previewResult = $state<ArtistMergePreviewResponse | null>(null);
	let previewRequest = $state<{
		source_artist_ids: string[];
		surviving_artist_id: string;
		expected_revisions: Record<string, number>;
	} | null>(null);
	let mergeOpen = $state(false);
	let dismissOpen = $state(false);
	let undoOpen = $state(false);

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

	const basisLabels: Record<NonNullable<ArtistDuplicateGroupSummary['merge_basis']>, string> = {
		musicbrainz_proof: 'MusicBrainz proof',
		same_name: 'Same name',
		v2_placeholder: 'v2 upgrade'
	};

	function stateClass(state: ArtistReconciliationGroupState): string {
		if (state === 'resolved_automatically') return 'badge-success';
		if (state === 'waiting_for_identity') return 'badge-info';
		if (state === 'same_name_only') return 'badge-ghost';
		return 'badge-warning';
	}

	function compactId(value: string): string {
		return value.length > 18 ? `${value.slice(0, 8)}…${value.slice(-6)}` : value;
	}

	function updateUrl(options: { clearGroup?: boolean; useDraftFilters?: boolean } = {}): void {
		const params = new SvelteURLSearchParams();
		const nextFilterState = options.useDraftFilters ? filterState : appliedFilterState;
		const nextSearch = options.useDraftFilters ? search.trim() : appliedSearch.trim();
		if (nextFilterState) params.set('state', nextFilterState);
		if (nextSearch) params.set('q', nextSearch);
		if (!options.clearGroup && selectedGroupId) params.set('group', selectedGroupId);
		void goto(
			withBasePath(`/library/management/artists${params.size ? `?${params.toString()}` : ''}`),
			{
				noScroll: true,
				keepFocus: true,
				replaceState: true
			}
		);
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

	async function openMerge(): Promise<void> {
		const request = mergeRequest();
		if (!request) return;
		providerChoice = 'retain_survivor';
		confirmed = false;
		stalePreview = false;
		previewResult = null;
		previewRequest = request;
		previewSequence += 1;
		try {
			previewResult = await previewArtist.mutateAsync({
				...request,
				provider_choice: providerChoice
			});
			mergeOpen = true;
		} catch {
			previewResult = null;
			previewRequest = null;
		}
	}

	// The preview token covers the provider choice, so a changed choice is
	// previewed again before it can be applied.
	// Only the latest request's answer counts: a slower, older preview
	// must not replace the one for the current choice.
	let previewSequence = 0;
	async function chooseProvider(choice: 'detach' | 'retain_survivor'): Promise<void> {
		if (!previewRequest) return;
		providerChoice = choice;
		confirmed = false;
		previewResult = null;
		const request = previewRequest;
		const sequence = ++previewSequence;
		try {
			const result = await previewArtist.mutateAsync({ ...request, provider_choice: choice });
			if (sequence === previewSequence) previewResult = result;
		} catch {
			if (sequence === previewSequence) previewResult = null;
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
			mergeOpen = false;
			selectedGroupId = null;
			updateUrl({ clearGroup: true });
		} catch {
			confirmed = false;
			stalePreview = true;
			previewResult = null;
		}
	}

	async function dismissGroup(): Promise<void> {
		if (!detail) return;
		await dismissMutation.mutateAsync({
			groupId: detail.id,
			expectedMemberRevisions: detail.member_revisions
		});
		selectedGroupId = null;
		updateUrl({ clearGroup: true });
	}

	async function undoJoin(): Promise<void> {
		if (!detail) return;
		await undoMutation.mutateAsync({ groupId: detail.id });
		selectedGroupId = null;
		updateUrl({ clearGroup: true });
	}

	const previewArtist = createPreviewArtistMergeMutation();
	const applyArtist = createApplyArtistMergeMutation();
	const dismissMutation = createDismissArtistDuplicateGroupMutation();
	const undoMutation = createUndoArtistJoinMutation();
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
			<p class="mt-1 max-w-3xl text-sm text-muted">
				MusicBrainz proof merges artists. Records with the same name join by themselves, and you can
				undo that here. Other similar names never merge.
			</p>
		</div>
		<Button href={withBasePath('/library/management?tab=scanning')} variant="ghost" size="sm"
			>Back to Scan &amp; identify</Button
		>
	</header>

	{#if progressQuery.isLoading}
		<div class="skeleton h-32 rounded-box"></div>
	{:else if progressQuery.isError}
		<div class="alert alert-error">Could not load artist reconciliation progress.</div>
	{:else if progressQuery.data}
		<Card as="section" variant="bordered" padding="md" aria-label="Artist identity counts">
			<div class="grid grid-cols-2 gap-3 text-sm sm:grid-cols-4">
				<div>
					<span class="block text-xs text-muted">Resolved automatically</span><strong
						>{progressQuery.data.automatically_resolved_count.toLocaleString()}</strong
					>
				</div>
				<div>
					<span class="block text-xs text-muted">Waiting for identity</span><strong
						>{progressQuery.data.waiting_for_identity_count.toLocaleString()}</strong
					>
				</div>
				<div>
					<span class="block text-xs text-muted">Needs judgement</span><strong
						>{progressQuery.data.genuine_review_count.toLocaleString()}</strong
					>
				</div>
				<div>
					<span class="block text-xs text-muted">Provider conflicts</span><strong
						>{progressQuery.data.provider_conflict_count.toLocaleString()}</strong
					>
				</div>
			</div>
		</Card>
	{/if}

	<Card variant="bordered" padding="sm">
		<form
			class="flex flex-wrap gap-2"
			onsubmit={(event) => {
				event.preventDefault();
				applyFilters();
			}}
		>
			<Input
				icon={Search}
				label="Search artist groups"
				class="min-w-64 flex-1"
				bind:value={search}
				placeholder="Search names or local artists"
			/>
			<label class="select flex items-center gap-2">
				<ListFilter class="h-4 w-4 text-muted" />
				<select bind:value={filterState} aria-label="Evidence state">
					{#each stateOptions as option (option.value)}
						<option value={option.value}>{option.label}</option>
					{/each}
				</select>
			</label>
			<Button type="submit" variant="primary">Apply filters</Button>
		</form>
	</Card>

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
			<p class="mx-auto mt-1 max-w-lg text-sm text-muted">Try changing the filters.</p>
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
								<p class="mt-1 text-sm text-muted">
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
					<Button
						variant="outline"
						class="w-full"
						disabled={groupsQuery.isFetchingNextPage}
						onclick={() => void groupsQuery.fetchNextPage()}
					>
						{groupsQuery.isFetchingNextPage ? 'Loading…' : 'Load more groups'}
					</Button>
				{/if}
			</div>

			{#snippet identityInspector()}
				<aside class={isWide ? 'sticky top-24' : ''} aria-label="Artist identity inspector">
					{#if !selectedSummary}
						<Card variant="bordered" class="text-center">
							<FingerprintPattern class="mx-auto h-8 w-8 text-base-content/35" />
							<p class="mt-3 font-semibold">Select a dossier</p>
							<p class="mt-1 text-sm text-muted">Inspect evidence and persisted references here.</p>
						</Card>
					{:else if detailQuery.isLoading}
						<div class="skeleton h-96 rounded-box"></div>
					{:else if detailQuery.isError || !detail}
						<div class="alert alert-error">Could not load this artist dossier.</div>
					{:else}
						<Card
							variant="bordered"
							padding="none"
							class={isWide ? 'max-h-[calc(100vh-8rem)] overflow-y-auto' : ''}
						>
							<header class="border-b border-base-content/10 p-4">
								<div class="flex items-start justify-between gap-3">
									<div>
										<p class="font-mono text-xs uppercase tracking-wide text-muted">
											Evidence dossier
										</p>
										<h2 class="mt-1 font-display text-xl font-bold">{detail.display_name}</h2>
									</div>
									<span class="badge {stateClass(detail.state)}">{stateLabel(detail.state)}</span>
								</div>
								<p class="mt-2 text-sm text-muted">{detail.reason.message}</p>
								<p class="mt-1 text-sm text-muted">{detail.reason.action}</p>
								{#if detail.merge_basis}
									<p class="mt-2 text-sm">
										<span class="text-muted">Merged on</span>
										<strong>{basisLabels[detail.merge_basis]}</strong>
									</p>
								{/if}
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
														><strong>{member.name}</strong>{#if member.disambiguation}<span
																class="block text-xs text-muted">{member.disambiguation}</span
															>{/if}<span class="block break-all font-mono text-xs text-muted"
															>{member.id} · revision {member.row_revision}</span
														></span
													>
												</span>
												<span class="mt-2 block text-xs text-muted">
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
											<div class="rounded-lg bg-base-100 p-2">
												<dt class="text-xs text-muted">{kind.replaceAll('_', ' ')}</dt>
												<dd class="font-semibold">{count}</dd>
											</div>
										{/each}
									</dl>
								</section>

								<section aria-labelledby="provider-evidence-title">
									<h3 id="provider-evidence-title" class="flex items-center gap-2 font-semibold">
										<FingerprintPattern class="h-4 w-4" /> Exact provider evidence
									</h3>
									{#if detail.evidence.length}
										<ul class="mt-2 space-y-2 text-sm">
											{#each detail.evidence as item (`${item.subject_kind}:${item.subject_id}:${item.artist_mbid}`)}
												<li class="rounded-lg bg-base-100 p-2">
													<strong>{item.subject_name}</strong>
													<span class="block text-xs text-muted"
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
											<ShieldQuestionMark class="h-4 w-4 shrink-0" /> No complete provider proof is stored
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
														>{release.name}<span class="block text-xs text-muted"
															>{release.identity_ready
																? release.exact_track_mapping_ready
																	? 'Exact release and tracks matched'
																	: 'Needs exact track mapping'
																: 'Needs release identification'}</span
														></span
													>
													<IconButton
														size="xs"
														shape="square"
														icon={ArrowUpRight}
														label={`Open ${release.name}`}
														href={withBasePath(`/album/${release.id}`)}
													/>
												</li>
											{/each}
										</ul>
									</section>
								{/if}

								{#if detail.can_undo}
									<Button
										variant="ghost"
										icon={Undo2}
										disabled={undoMutation.isPending}
										onclick={() => (undoOpen = true)}>Undo this join</Button
									>
								{/if}

								{#if detail.state !== 'resolved_automatically'}
									<div class="grid gap-2">
										<Button
											variant="primary"
											icon={Merge}
											disabled={!survivingId || previewArtist.isPending}
											onclick={() => void openMerge()}>Preview group merge</Button
										>
										<Button
											variant="ghost"
											icon={Split}
											disabled={dismissMutation.isPending}
											onclick={() => (dismissOpen = true)}>Mark records as distinct</Button
										>
									</div>
								{/if}
							</div>
						</Card>
					{/if}
				</aside>
			{/snippet}

			{#if isWide}
				{@render identityInspector()}
			{/if}
		</div>
	{/if}
</section>

<Modal bind:open={mergeOpen} title="Confirm artist group merge" size="lg" focusTitle>
	{#if stalePreview}
		<div class="alert alert-warning">
			<CircleAlert class="h-4 w-4" /> The records changed after preview. Close this dialog and create
			a fresh preview.
		</div>
	{:else if previewResult}
		<p class="text-sm text-muted">
			{previewResult.aliases.length} retired IDs will remain aliases. Files, tags, artwork, and paths
			are untouched.
		</p>
		<div class="mt-4 grid gap-3 sm:grid-cols-2" aria-label="Artist merge identities">
			<div class="rounded-box border border-primary/25 bg-primary/5 p-3">
				<p class="text-xs font-semibold uppercase tracking-wide text-primary">Chosen survivor</p>
				{#if chosenSurvivor}
					<p class="mt-1 font-semibold">{chosenSurvivor.name}</p>
					<p class="break-all font-mono text-xs text-muted">
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
				<p class="text-xs font-semibold uppercase tracking-wide text-muted">
					Retired records · {retiredMembers.length}
				</p>
				<ul class="mt-1 space-y-1">
					{#each retiredMembers as member (member.id)}
						<li>
							<span class="font-semibold">{member.name}</span>
							<span class="block break-all font-mono text-xs text-muted">
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
					<dt class="text-xs text-muted">{kind.replaceAll('_', ' ')}</dt>
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
						name="provider-choice"
						checked={providerChoice === 'retain_survivor'}
						onchange={() => void chooseProvider('retain_survivor')}
					/> Keep the chosen survivor's provider identity</label
				>
				<label class="mt-2 flex gap-2 text-sm"
					><input
						type="radio"
						class="radio radio-sm"
						name="provider-choice"
						checked={providerChoice === 'detach'}
						onchange={() => void chooseProvider('detach')}
					/> Detach conflicting provider identities</label
				>
			</fieldset>
		{/if}
		<label class="mt-4 flex items-start gap-2 text-sm"
			><input type="checkbox" class="checkbox checkbox-sm mt-0.5" bind:checked={confirmed} /> Merge all
			records in this dossier and preserve retired IDs as aliases.</label
		>
	{/if}
	{#snippet actions()}
		<Button variant="ghost" onclick={() => (mergeOpen = false)}>Cancel</Button>
		<Button
			variant="primary"
			disabled={!previewResult || !confirmed || applyArtist.isPending || previewArtist.isPending}
			onclick={() => void mergeArtists()}>Merge artists</Button
		>
	{/snippet}
</Modal>

<ConfirmDialog
	bind:open={dismissOpen}
	title="Keep these artists distinct?"
	message="This dismisses the current record revisions as a candidate group. If any of these records changes, the evidence can be reviewed again."
	confirmLabel="Mark as distinct"
	focusTitle
	pending={dismissMutation.isPending}
	onconfirm={dismissGroup}
/>

<ConfirmDialog
	bind:open={undoOpen}
	title="Split these artists apart?"
	message="Each record gets back its songs, plays, favorites and ratings, and they stay apart from now on. Anything added since the join stays where it is."
	confirmLabel="Undo join"
	focusTitle
	pending={undoMutation.isPending}
	onconfirm={undoJoin}
/>
