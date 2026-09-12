<script lang="ts">
	import {
		TriangleAlert,
		BadgeCheck,
		Check,
		Clock3,
		ExternalLink,
		GitCompareArrows,
		Library,
		LoaderCircle,
		RefreshCw,
		SearchCheck,
		ShieldCheck
	} from 'lucide-svelte';
	import type { ContributionDuplicateCandidate, LibraryContribution } from '$lib/types';
	import { toastStore } from '$lib/stores/toast';
	import {
		attachExistingMusicBrainzReleaseMutation,
		checkMusicBrainzDuplicatesMutation,
		createMusicBrainzSeedMutation,
		recordMusicBrainzResultMutation,
		retryMusicBrainzVerificationMutation
	} from '$lib/queries/libraryContributions/LibraryContributionMutations.svelte';
	import {
		MUSICBRAINZ_RELEASE_EDITOR,
		parseMusicBrainzReleaseId,
		postMusicBrainzSeed
	} from '$lib/utils/musicBrainzSeedForm';

	interface Props {
		contribution: LibraryContribution;
		canMutate?: boolean;
	}

	let { contribution, canMutate = true }: Props = $props();
	const checkMutation = checkMusicBrainzDuplicatesMutation();
	const attachMutation = attachExistingMusicBrainzReleaseMutation();
	const seedMutation = createMusicBrainzSeedMutation();
	const resultMutation = recordMusicBrainzResultMutation();
	const retryMutation = retryMusicBrainzVerificationMutation();
	let differentEdition = $state(false);
	let recoveryValue = $state('');
	let recoveryTouched = $state(false);

	const result = $derived(contribution.duplicate_result);
	const exactCandidates = $derived(result?.candidates.filter((candidate) => candidate.exact) ?? []);
	const reviewCandidates = $derived(
		result?.candidates.filter((candidate) => !candidate.exact) ?? []
	);
	const seriousCandidates = $derived(
		reviewCandidates.filter((candidate) => ['barcode', 'similar'].includes(candidate.evidence_kind))
	);
	const groupCandidates = $derived(
		reviewCandidates.filter((candidate) => candidate.evidence_kind === 'release_group')
	);
	const duplicateCheckComplete = $derived(
		Boolean(result && result.input_revision === contribution.input_revision)
	);
	const canSeed = $derived(
		contribution.next_actions.includes('seed_musicbrainz') || contribution.state === 'seeded'
	);
	const parsedRecoveryMbid = $derived(parseMusicBrainzReleaseId(recoveryValue));

	function checkDuplicates(confirmDifferentEdition = false): void {
		if (!canMutate || checkMutation.isPending) return;
		checkMutation.mutate({
			contributionId: contribution.id,
			expectedRowRevision: contribution.row_revision,
			differentEditionConfirmed: confirmDifferentEdition
		});
	}

	function attach(candidate: ContributionDuplicateCandidate): void {
		if (!canMutate || !candidate.release_mbid || attachMutation.isPending) return;
		attachMutation.mutate({
			contributionId: contribution.id,
			expectedRowRevision: contribution.row_revision,
			releaseMbid: candidate.release_mbid
		});
	}

	async function continueOnMusicBrainz(): Promise<void> {
		if (!canMutate || !canSeed || seedMutation.isPending) return;
		const target = `droppedneedle-musicbrainz-${contribution.id}`;
		const editorWindow = window.open('about:blank', target);
		if (!editorWindow) {
			toastStore.show({
				message: 'Allow pop-ups for DroppedNeedle, then try again',
				type: 'error'
			});
			return;
		}
		editorWindow.opener = null;
		editorWindow.document.title = 'Opening MusicBrainz…';
		editorWindow.document.body.textContent = 'Preparing the MusicBrainz release editor…';

		try {
			const seed = await seedMutation.mutateAsync({
				contributionId: contribution.id,
				expectedRowRevision: contribution.row_revision
			});
			postMusicBrainzSeed(seed, target);
		} catch {
			editorWindow.close();
		}
	}

	function recordResult(): void {
		recoveryTouched = true;
		if (!canMutate || !parsedRecoveryMbid || resultMutation.isPending) return;
		resultMutation.mutate({
			contributionId: contribution.id,
			expectedRowRevision: contribution.row_revision,
			releaseIdOrUrl: parsedRecoveryMbid,
			replaceExistingResult: Boolean(
				contribution.result_release_mbid && contribution.result_release_mbid !== parsedRecoveryMbid
			)
		});
	}

	function retryVerification(): void {
		if (!canMutate || retryMutation.isPending) return;
		retryMutation.mutate({
			contributionId: contribution.id,
			expectedRowRevision: contribution.row_revision
		});
	}

	function candidateUrl(candidate: ContributionDuplicateCandidate): string | null {
		if (candidate.release_mbid) {
			return `https://musicbrainz.org/release/${candidate.release_mbid}`;
		}
		if (candidate.release_group_mbid) {
			return `https://musicbrainz.org/release-group/${candidate.release_group_mbid}`;
		}
		return null;
	}

	function evidenceLabel(candidate: ContributionDuplicateCandidate): string {
		return {
			exact_discogs_url: 'Exact Discogs link',
			release_group: 'Existing release group',
			barcode: 'Same barcode',
			similar: 'Similar release'
		}[candidate.evidence_kind];
	}
</script>

{#snippet recoveryForm()}
	<div class="rounded-box border border-base-content/10 bg-base-200/45 p-4">
		<h3 class="font-semibold">Already submitted the release?</h3>
		<p class="mt-1 text-sm text-base-content/60">
			If MusicBrainz did not return you here, paste the release MBID or its full MusicBrainz release
			URL.
		</p>
		<div class="mt-3 flex flex-col gap-2 sm:flex-row">
			<label class="form-control min-w-0 flex-1">
				<span class="sr-only">MusicBrainz release MBID or URL</span>
				<input
					bind:value={recoveryValue}
					class="input input-bordered w-full font-mono text-sm"
					class:input-error={recoveryTouched && recoveryValue.length > 0 && !parsedRecoveryMbid}
					placeholder="Release MBID or https://musicbrainz.org/release/…"
					onblur={() => (recoveryTouched = true)}
					disabled={!canMutate}
				/>
			</label>
			<button
				class="btn btn-secondary gap-2"
				disabled={!canMutate || !parsedRecoveryMbid || resultMutation.isPending}
				onclick={recordResult}
			>
				{#if resultMutation.isPending}<LoaderCircle
						class="h-4 w-4 animate-spin"
					/>{:else}<SearchCheck class="h-4 w-4" />{/if}
				{contribution.result_release_mbid && parsedRecoveryMbid !== contribution.result_release_mbid
					? 'Replace and verify'
					: 'Verify result'}
			</button>
		</div>
		{#if recoveryTouched && recoveryValue.length > 0 && !parsedRecoveryMbid}
			<p class="mt-2 text-sm text-error" role="alert">
				Enter a release MBID or an official musicbrainz.org release URL.
			</p>
		{/if}
	</div>
{/snippet}

<section
	class="overflow-hidden rounded-box border border-base-content/10 bg-base-100"
	aria-labelledby="musicbrainz-review-title"
>
	<div class="flex items-start gap-3 border-b border-base-content/10 px-5 py-4 sm:px-6">
		<div
			class="grid h-10 w-10 shrink-0 place-items-center rounded-box bg-secondary/10 text-secondary"
		>
			<SearchCheck class="h-5 w-5" />
		</div>
		<div>
			<h2 id="musicbrainz-review-title" class="font-bold">Check MusicBrainz</h2>
			<p class="text-sm text-base-content/55">
				Make sure this release does not already exist before opening the editor.
			</p>
		</div>
	</div>

	{#if contribution.state === 'linked'}
		<div class="p-5 sm:p-6">
			<div class="flex items-start gap-3 rounded-box border border-success/25 bg-success/10 p-4">
				<BadgeCheck class="mt-0.5 h-5 w-5 shrink-0 text-success" />
				<div>
					<h3 class="font-bold">Linked to MusicBrainz</h3>
					<p class="mt-1 text-sm text-base-content/65">
						This local album keeps its DroppedNeedle identity and now has a verified MusicBrainz
						match.
					</p>
					{#if contribution.result_release_mbid}
						<a
							class="link link-primary mt-2 inline-flex items-center gap-1 text-sm font-semibold"
							href={`https://musicbrainz.org/release/${contribution.result_release_mbid}`}
							target="_blank"
							rel="noreferrer"
						>
							View release on MusicBrainz <ExternalLink class="h-3.5 w-3.5" />
						</a>
					{/if}
				</div>
			</div>
		</div>
	{:else if contribution.state === 'verifying'}
		<div class="p-5 sm:p-6">
			<div class="flex items-start gap-3 rounded-box border border-info/25 bg-info/10 p-4 sm:p-5">
				<Clock3 class="mt-0.5 h-5 w-5 shrink-0 text-info" />
				<div class="min-w-0">
					<h3 class="font-bold">MusicBrainz release returned</h3>
					<p class="mt-1 text-sm text-base-content/65">
						DroppedNeedle is waiting for the release to appear in the MusicBrainz API. It will
						compare the release with the current local album before attaching anything.
					</p>
					{#if contribution.result_release_mbid}
						<a
							class="link link-primary mt-3 inline-flex max-w-full items-center gap-1 break-all font-mono text-xs"
							href={`https://musicbrainz.org/release/${contribution.result_release_mbid}`}
							target="_blank"
							rel="noreferrer"
						>
							{contribution.result_release_mbid}
							<ExternalLink class="h-3.5 w-3.5 shrink-0" />
						</a>
					{/if}
					<p class="mt-3 flex items-center gap-2 text-xs text-base-content/50">
						<LoaderCircle class="h-3.5 w-3.5 animate-spin" /> This page updates automatically.
					</p>
				</div>
			</div>
		</div>
	{:else if contribution.state === 'needs_review' && contribution.result_release_mbid}
		<div class="space-y-4 p-5 sm:p-6">
			<div class="flex items-start gap-3 rounded-box border border-warning/35 bg-warning/10 p-4">
				<TriangleAlert class="mt-0.5 h-5 w-5 shrink-0 text-warning" />
				<div class="min-w-0 flex-1">
					<h3 class="font-bold">The returned release needs review</h3>
					<p class="mt-1 text-sm text-base-content/65">
						Nothing was attached. The release was unavailable for too long or did not pass the
						current album evidence checks.
					</p>
					<a
						class="link link-primary mt-2 inline-flex items-center gap-1 text-sm font-semibold"
						href={`https://musicbrainz.org/release/${contribution.result_release_mbid}`}
						target="_blank"
						rel="noreferrer"
					>
						Review on MusicBrainz <ExternalLink class="h-3.5 w-3.5" />
					</a>
					<div class="mt-4">
						<button
							class="btn btn-warning btn-sm gap-2"
							disabled={!canMutate || retryMutation.isPending}
							onclick={retryVerification}
						>
							{#if retryMutation.isPending}<LoaderCircle
									class="h-4 w-4 animate-spin"
								/>{:else}<RefreshCw class="h-4 w-4" />{/if}
							Retry verification
						</button>
					</div>
				</div>
			</div>
			{@render recoveryForm()}
		</div>
	{:else if !duplicateCheckComplete}
		<div class="p-5 sm:p-6">
			<div class="grid gap-5 md:grid-cols-[minmax(0,1fr)_auto] md:items-center">
				<div>
					<h3 class="font-bold">Search before you add</h3>
					<p class="mt-1 max-w-2xl text-sm text-base-content/60">
						DroppedNeedle checks Discogs relationships, release groups, barcodes and similar
						releases. An exact Discogs match blocks a duplicate submission.
					</p>
				</div>
				<button
					class="btn btn-secondary gap-2"
					disabled={!canMutate || checkMutation.isPending || contribution.validation.length > 0}
					onclick={() => checkDuplicates()}
				>
					{#if checkMutation.isPending}
						<LoaderCircle class="h-4 w-4 animate-spin" /> Checking…
					{:else}
						<SearchCheck class="h-4 w-4" /> Check MusicBrainz
					{/if}
				</button>
			</div>
		</div>
	{:else}
		<div class="space-y-5 p-5 sm:p-6">
			{#if exactCandidates.length === 1}
				{@const candidate = exactCandidates[0]}
				<div class="rounded-box border border-warning/35 bg-warning/10 p-4 sm:p-5">
					<div class="flex items-start gap-3">
						<TriangleAlert class="mt-0.5 h-5 w-5 shrink-0 text-warning" />
						<div class="min-w-0 flex-1">
							<p class="text-xs font-bold uppercase tracking-[0.14em] text-warning">
								Exact match found
							</p>
							<h3 class="mt-1 text-lg font-bold">{candidate.title}</h3>
							<p class="text-sm text-base-content/65">{candidate.artist_name}</p>
							<p class="mt-2 text-sm text-base-content/65">
								Discogs already links this release to MusicBrainz. Creating another release is
								blocked.
							</p>
							<div class="mt-4 flex flex-wrap gap-2">
								<button
									class="btn btn-warning btn-sm gap-2"
									disabled={!canMutate || attachMutation.isPending || !candidate.release_mbid}
									onclick={() => attach(candidate)}
								>
									{#if attachMutation.isPending}<LoaderCircle
											class="h-4 w-4 animate-spin"
										/>{:else}<Library class="h-4 w-4" />{/if}
									Use this MusicBrainz release
								</button>
								{#if candidateUrl(candidate)}
									<a
										class="btn btn-ghost btn-sm gap-2"
										href={candidateUrl(candidate) ?? undefined}
										target="_blank"
										rel="noreferrer"
									>
										Inspect on MusicBrainz <ExternalLink class="h-3.5 w-3.5" />
									</a>
								{/if}
							</div>
						</div>
					</div>
				</div>
			{:else if exactCandidates.length > 1}
				<div class="rounded-box border border-error/30 bg-error/10 p-4">
					<div class="flex items-start gap-3">
						<TriangleAlert class="mt-0.5 h-5 w-5 shrink-0 text-error" />
						<div>
							<h3 class="font-bold">Discogs points to several MusicBrainz releases</h3>
							<p class="mt-1 text-sm text-base-content/65">
								DroppedNeedle cannot safely choose one. Review the relationships upstream, then run
								the check again.
							</p>
						</div>
					</div>
				</div>
			{:else}
				<div class="flex items-start gap-3 rounded-box border border-success/25 bg-success/10 p-4">
					<ShieldCheck class="mt-0.5 h-5 w-5 shrink-0 text-success" />
					<div>
						<h3 class="font-bold">No exact release found</h3>
						<p class="mt-1 text-sm text-base-content/65">
							No exact match was found. Review any similar releases below before continuing.
						</p>
					</div>
				</div>
			{/if}

			{#if reviewCandidates.length > 0}
				<div>
					<div class="mb-3 flex items-center gap-2">
						<GitCompareArrows class="h-4 w-4 text-base-content/45" />
						<h3 class="font-bold">Possible existing entries</h3>
					</div>
					<div class="divide-y divide-base-content/10 rounded-box border border-base-content/10">
						{#each reviewCandidates as candidate, index (`${candidate.release_mbid ?? candidate.release_group_mbid}-${index}`)}
							<div class="grid gap-3 p-4 sm:grid-cols-[minmax(0,1fr)_auto] sm:items-start">
								<div class="min-w-0">
									<span class="badge badge-outline badge-sm">{evidenceLabel(candidate)}</span>
									<p class="mt-2 font-semibold">
										{candidate.title || contribution.draft.title.value}
									</p>
									<p class="text-sm text-base-content/55">{candidate.artist_name}</p>
									{#if candidate.differences.length}
										<ul class="mt-2 list-inside list-disc text-sm text-base-content/60">
											{#each candidate.differences as difference (difference)}
												<li>{difference}</li>
											{/each}
										</ul>
									{/if}
								</div>
								{#if candidateUrl(candidate)}
									<a
										class="btn btn-ghost btn-xs gap-1"
										href={candidateUrl(candidate) ?? undefined}
										target="_blank"
										rel="noreferrer"
									>
										Review <ExternalLink class="h-3 w-3" />
									</a>
								{/if}
							</div>
						{/each}
					</div>
				</div>
			{/if}

			{#if exactCandidates.length === 0 && seriousCandidates.length > 0 && !result?.different_edition_confirmed}
				<div class="rounded-box border border-base-content/15 bg-base-200/45 p-4">
					<label class="flex cursor-pointer items-start gap-3">
						<input
							bind:checked={differentEdition}
							type="checkbox"
							class="checkbox checkbox-primary mt-0.5"
						/>
						<span>
							<span class="block font-semibold">These are different editions</span>
							<span class="mt-1 block text-sm text-base-content/60">
								I reviewed the candidates and this local release should be a separate MusicBrainz
								entry.
							</span>
						</span>
					</label>
					<button
						class="btn btn-primary btn-sm mt-4 gap-2"
						disabled={!canMutate || !differentEdition || checkMutation.isPending}
						onclick={() => checkDuplicates(true)}
					>
						{#if checkMutation.isPending}<LoaderCircle class="h-4 w-4 animate-spin" />{:else}<Check
								class="h-4 w-4"
							/>{/if}
						Confirm and check again
					</button>
				</div>
			{/if}

			{#if canSeed && exactCandidates.length === 0}
				<div class="overflow-hidden rounded-box border border-primary/25 bg-primary/5">
					<div class="grid gap-px bg-base-content/10 sm:grid-cols-3">
						<div class="bg-base-100/90 p-4">
							<span class="text-xs text-base-content/45">Release</span>
							<p class="mt-1 font-bold">{contribution.draft.title.value}</p>
						</div>
						<div class="bg-base-100/90 p-4">
							<span class="text-xs text-base-content/45">Artist credit</span>
							<p class="mt-1 font-bold">{contribution.draft.artist_credit.value}</p>
						</div>
						<div class="bg-base-100/90 p-4">
							<span class="text-xs text-base-content/45">Destination</span>
							<p class="mt-1 font-bold">Official MusicBrainz editor</p>
						</div>
					</div>
					<div class="p-4 sm:flex sm:items-center sm:justify-between sm:gap-5 sm:p-5">
						<div>
							<h3 class="font-bold">Ready for your final review</h3>
							<p class="mt-1 max-w-2xl text-sm text-base-content/60">
								You will sign in, review every field and submit on MusicBrainz. DroppedNeedle does
								not receive your MusicBrainz password.
							</p>
							{#if groupCandidates.length === 1}
								<p class="mt-2 text-sm font-semibold text-primary">
									The editor will use the existing release group found above.
								</p>
							{/if}
						</div>
						<button
							class="btn btn-primary mt-4 shrink-0 gap-2 sm:mt-0"
							disabled={!canMutate || seedMutation.isPending}
							onclick={continueOnMusicBrainz}
						>
							{#if seedMutation.isPending}<LoaderCircle
									class="h-4 w-4 animate-spin"
								/>{:else}<ExternalLink class="h-4 w-4" />{/if}
							{contribution.state === 'seeded' ? 'Continue again' : 'Continue on MusicBrainz'}
						</button>
					</div>
					<div
						class="border-t border-base-content/10 px-4 py-3 text-xs text-base-content/50 sm:px-5"
					>
						Submits a one-time form to
						<a class="link" href={MUSICBRAINZ_RELEASE_EDITOR}>musicbrainz.org</a>. Your local files
						are never changed.
					</div>
				</div>
				{@render recoveryForm()}
			{/if}
		</div>
	{/if}
</section>
